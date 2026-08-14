//! X11 window-manager backend.

mod agent;
mod semantic;
mod session;

pub use session::{SessionError, SessionRestore, SessionSnapshot};

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, VecDeque},
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use nobox_agent_wire::{
    ActionId as AgentActionId, CapabilitySet as AgentCapabilities,
    ClientMessage as AgentClientMessage, ErrorCode as AgentErrorCode, MAX_CAPTURE_PIXELS,
    Outcome as AgentOutcome, ProtocolError as AgentError, Reply as AgentReply,
    RequestId as AgentRequestId, ServerMessage as AgentServerMessage, SessionId as AgentSessionId,
    Step as AgentStep,
};
use nobox_config::{
    Action, ActionQuery, ActionQueryContext, ActionQueryTarget, ApplicationIdentity,
    ApplicationKind, ApplicationLayer, ApplicationMatcher, ApplicationSettings,
    ApplicationWorkspace, AxisPosition, Config, EdgeDirection, KeyboardModifier, LayerTarget,
    MAX_COMMAND_MENU_BYTES, MAX_WORKSPACES, MaximizeDirection, MenuDefinition, MenuEntry,
    MenuSource, MouseContext, MouseModifier, MouseTrigger, OutputTarget, PositiveRelativeAmount,
    ResizeEdge, RgbColor, ScreenshotTarget, SizeBasis, StartupNotification, ThemeConfig,
    TitleAlignment, WindowDirection, WorkspacePlacement,
};
use nobox_core::{
    AspectRange, AspectRatio, AxisPlacement, BlockingEdgePolicy, CardinalDirection, Client,
    ClientDecorations, ClientId, ClientLayer, ClientPolicy, ClientPresentation, ClientRole,
    ClientSet, DecorationExtents, DecorationOverride, EdgeReservation, EdgeReservations, Geometry,
    Gravity, Output, OutputCoverage, OutputId, OutputSet, ResizeDeltas, RestackDecision, Size,
    SizeHints, SpatialDirection, TransientTarget, WorkspaceAssignment, WorkspaceCorner,
    WorkspaceDirection, WorkspaceId, WorkspaceLayout, WorkspaceOrientation, adaptive_restack,
    agent::{
        AgentState, AgentVisibility as AgentClientVisibility, ClientDetails as AgentClientDetails,
        Grant as AgentGrant,
    },
    centered_placement, directional_grow_geometry, directional_move_geometry,
    directional_shrink_geometry, directional_target, grow_to_fill_geometry, move_resize_geometry,
    relative_resize_geometry, smart_placement,
};
use nobox_desktop::{ApplicationCatalog, DesktopApplication, LaunchCommand};
use thiserror::Error;
use tracing::{debug, info, warn};
use x11rb::protocol::composite::ConnectionExt as _;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::{
    COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE,
    connection::{Connection, RequestConnection},
    errors::{ConnectError, ConnectionError, ReplyError, ReplyOrIdError},
    properties::{WmHints, WmHintsState, WmSizeHints},
    protocol::{
        ErrorKind, Event,
        randr::{ConnectionExt as _, NotifyMask},
        res::{ClientIdMask, ClientIdSpec, ClientIdValue, ConnectionExt as _},
        shape::{ConnectionExt as _, SK, SO},
        sync::{
            self, Alarm, ConnectionExt as _, Counter, CreateAlarmAux, Int64 as SyncInt64, TESTTYPE,
            VALUETYPE,
        },
        xproto::{
            Allow, AtomEnum, BackPixmap, ButtonIndex, ButtonPressEvent, ButtonReleaseEvent,
            CONFIGURE_NOTIFY_EVENT, ChangeGCAux, ChangeWindowAttributesAux, Charinfo,
            ClientMessageEvent, ClipOrdering, Colormap, ColormapNotifyEvent, ConfigWindow,
            ConfigureNotifyEvent, ConfigureRequestEvent, ConfigureWindowAux, ConnectionExt as _,
            CreateGCAux, CreateWindowAux, Cursor, EnterNotifyEvent, EventMask, FocusInEvent, Font,
            Gcontext, Grab, GrabMode, GrabStatus, InputFocus, KeyPressEvent, KeyReleaseEvent,
            LeaveNotifyEvent, MapState, ModMask, MotionNotifyEvent, NotifyDetail, NotifyMode,
            QueryFontReply, Rectangle, SELECTION_NOTIFY_EVENT, Segment, SelectionNotifyEvent,
            SelectionRequestEvent, SetMode, StackMode, UnmapNotifyEvent, Window, WindowClass,
        },
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

/// Returns the X server's local process identity for the client owning `window`.
/// Client-authored `_NET_WM_PID` is deliberately not consulted.
fn xres_local_client_pid<C: Connection>(connection: &C, window: Window) -> Option<u32> {
    let version = connection.res_query_version(1, 2).ok()?.reply().ok()?;
    let spec = ClientIdSpec {
        client: window,
        mask: ClientIdMask::LOCAL_CLIENT_PID,
    };
    let reply = connection
        .res_query_client_ids(&[spec])
        .ok()?
        .reply()
        .ok()?;
    verified_xres_pid(
        version.server_major,
        version.server_minor,
        window,
        connection.setup().resource_id_mask,
        &reply.ids,
    )
}

/// Returns the X-Resource client namespace that owns `window`.
///
/// Unlike a PID, this distinguishes separate X11 connections made by one
/// process. That is the boundary used when a text target asks for a temporary
/// selection through one of its helper windows.
fn xres_client_base<C: Connection>(connection: &C, window: Window) -> Option<u32> {
    let version = connection.res_query_version(1, 2).ok()?.reply().ok()?;
    let reply = connection
        .res_query_client_ids(&[ClientIdSpec {
            client: window,
            mask: ClientIdMask::CLIENT_XID,
        }])
        .ok()?
        .reply()
        .ok()?;
    verified_xres_client_base(version.server_major, version.server_minor, &reply.ids)
}

fn verified_xres_pid(
    server_major: u16,
    server_minor: u16,
    window: Window,
    resource_id_mask: u32,
    ids: &[ClientIdValue],
) -> Option<u32> {
    if (server_major, server_minor) < (1, 2) {
        return None;
    }
    let client_base = window & !resource_id_mask;
    let mut matches = ids.iter().filter(|identity| {
        identity.spec.client == client_base && identity.spec.mask == ClientIdMask::LOCAL_CLIENT_PID
    });
    let identity = matches.next()?;
    if matches.next().is_some() || identity.value.len() != 1 {
        return None;
    }
    let pid = identity.value[0];
    (pid > 0 && pid <= i32::MAX as u32).then_some(pid)
}

fn verified_xres_client_base(
    server_major: u16,
    server_minor: u16,
    ids: &[ClientIdValue],
) -> Option<u32> {
    if (server_major, server_minor) < (1, 2) {
        return None;
    }
    let mut matches = ids.iter().filter(|identity| {
        identity.spec.mask == ClientIdMask::CLIENT_XID && identity.value.is_empty()
    });
    let client = matches.next()?.spec.client;
    (matches.next().is_none() && client != NONE).then_some(client)
}

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        CLIPBOARD,
        UTF8_STRING,
        TEXT_PLAIN: b"text/plain",
        TEXT_PLAIN_UTF8: b"text/plain;charset=utf-8",
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
        _AGENT_SEAT,
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
        _NET_WM_FULL_PLACEMENT,
        _NET_MOVERESIZE_WINDOW,
        _NET_WM_FULLSCREEN_MONITORS,
        _NET_WM_MOVERESIZE,
        _NET_NUMBER_OF_DESKTOPS,
        _NET_REQUEST_FRAME_EXTENTS,
        _NET_RESTACK_WINDOW,
        _NET_SHOWING_DESKTOP,
        _NET_SUPPORTED,
        _NET_SUPPORTING_WM_CHECK,
        _NET_STARTUP_INFO,
        _NET_STARTUP_INFO_BEGIN,
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
        _NET_WM_PID,
        _NET_WM_PING,
        _NET_WM_WINDOW_OPACITY,
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
        _NET_STARTUP_ID,
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
        | EventMask::ENTER_WINDOW
}

const WM_STATE_NORMAL: u32 = 1;
const WM_STATE_ICONIC: u32 = 3;
const CURSOR_BOTTOM_LEFT_CORNER: u16 = 12;
const CURSOR_BOTTOM_RIGHT_CORNER: u16 = 14;
const CURSOR_BOTTOM_SIDE: u16 = 16;
const CURSOR_MOVE: u16 = 52;
const CURSOR_POINTER: u16 = 68;
const CURSOR_LEFT_SIDE: u16 = 70;
const CURSOR_RIGHT_SIDE: u16 = 96;
const CURSOR_TOP_LEFT_CORNER: u16 = 134;
const CURSOR_TOP_RIGHT_CORNER: u16 = 136;
const CURSOR_TOP_SIDE: u16 = 138;
const RESIZE_HANDLE_SIZE: u32 = 8;
const FOCUS_INDICATOR_WIDTH: u32 = 6;
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
const CONTROL_SESSION_SAVE: u32 = 6;
const CONTROL_STARTUP_TIMEOUT: u32 = 7;
pub(crate) const CONTROL_AGENT_TRAFFIC: u32 = 8;
const CONTROL_AGENT_MARKER: u32 = 9;
const CONTROL_AGENT_OBSERVATION: u32 = 10;
pub(crate) const CONTROL_AGENT_SEMANTIC_READY: u32 = 11;
const CONTROL_AGENT_SEMANTIC_TIMEOUT: u32 = 12;
const CONTROL_AGENT_TEXT: u32 = 13;

/// X11 event types accepted by the test extension.
const KEY_PRESS_EVENT_TYPE: u8 = 2;
const KEY_RELEASE_EVENT_TYPE: u8 = 3;
const BUTTON_PRESS_EVENT_TYPE: u8 = 4;
const BUTTON_RELEASE_EVENT_TYPE: u8 = 5;
const MOTION_NOTIFY_EVENT_TYPE: u8 = 6;

/// How long an injected event stays claimable as the manager's own.
const INJECTION_PROVENANCE_WINDOW: Duration = Duration::from_millis(1_500);

/// Injections tracked at once; a burst beyond this drops the oldest.
const MAX_TRACKED_INJECTIONS: usize = 64;
/// Root children and input-shape rectangles inspected for one agent click.
///
/// Pointer input must fail closed when the X server's live hit-test state is
/// too large to inspect promptly. These limits are deliberately well above an
/// ordinary desktop while keeping one request bounded.
const MAX_AGENT_HIT_TEST_WINDOWS: usize = 4_096;
const MAX_AGENT_HIT_TEST_RECTANGLES: usize = 1_024;
const MAX_SEMANTIC_CLIENT_SCAN: usize = 256;
const MAX_SEMANTIC_CONTINUATIONS: usize = 16;
const SEMANTIC_REPLY_DELAY: Duration = Duration::from_millis(1_200);

/// Delay between complete character strokes sent through XTEST.
///
/// Rich editors need an event-loop boundary between strokes. Eight
/// milliseconds keeps a maximum-sized request below ordinary MCP timeouts
/// while avoiding the unbounded burst that caused dropped and repeated text.
const AGENT_TEXT_STROKE_DELAY: Duration = Duration::from_millis(8);

/// Largest request kept on the paced physical-key path.
const MAX_PACED_TEXT_SCALARS: usize = 4096;

/// Maximum time an exact UTF-8 offer remains available after the paste chord.
const AGENT_TEXT_TRANSFER_TIMEOUT: Duration = Duration::from_secs(2);

/// Quiet period after the latest completed text conversion.
///
/// Browsers may make follow-up conversions after the first UTF-8 response;
/// releasing CLIPBOARD immediately can acknowledge a paste without inserting
/// anything. A short, re-armed grace period lets that exchange finish while
/// the absolute transfer timeout remains in force.
const AGENT_TEXT_TRANSFER_QUIET: Duration = Duration::from_millis(250);

/// Shortest gap between two `human_activity` events.
const HUMAN_ACTIVITY_INTERVAL: Duration = Duration::from_millis(250);

/// How long a window stays marked after receiving agent input.
const AGENT_INPUT_MARKER_HOLD: Duration = Duration::from_millis(1_500);

/// Size of the standing agent-seat indicator.
const AGENT_INDICATOR_WIDTH: u16 = 96;
const AGENT_INDICATOR_HEIGHT: u16 = 16;
const CLIENT_PING_TIMEOUT: Duration = Duration::from_secs(3);
const SYNC_RESIZE_TIMEOUT: Duration = Duration::from_secs(1);
const PREFERRED_CLIENT_ICON_SIZE: u32 = 32;
const MAX_CLIENT_ICON_DIMENSION: u32 = 256;
const MAX_CLIENT_ICON_PROPERTY_VALUES: u32 = 256 * 256 + 2;
const MAX_SELECTION_MULTIPLE_PAIRS: u32 = 64;
const MAX_AGENT_ADVERTISEMENT_BYTES: usize = 256;
const MAX_AGENT_ADVERTISEMENT_LONGS: u32 = 65;
const MAX_CLIENT_COLORMAP_WINDOWS: usize = 256;
const PROCESS_REAP_INTERVAL: Duration = Duration::from_millis(250);
const STARTUP_SEQUENCE_TIMEOUT: Duration = Duration::from_secs(20);
const STARTUP_CHANGE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_STARTUP_MESSAGE_BYTES: usize = 4_096;
const MAX_STARTUP_MESSAGE_BUFFERS: usize = 64;
const MAX_STARTUP_SEQUENCES: usize = 256;
static STARTUP_SEQUENCE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Process-level action requested when the X11 event loop stops cleanly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunDisposition {
    /// Finish the window-manager process.
    Exit,
    /// Start a fresh nobox backend, or replace it with a configured command.
    Restart {
        /// Optional shell command to execute after releasing X11 ownership.
        command: Option<String>,
    },
}

/// Session state and process disposition produced by a clean X11 shutdown.
#[derive(Clone, Debug)]
pub struct RunOutcome {
    snapshot: SessionSnapshot,
    disposition: RunDisposition,
}

impl RunOutcome {
    /// Separates the captured session state from the requested process action.
    #[must_use]
    pub fn into_parts(self) -> (SessionSnapshot, RunDisposition) {
        (self.snapshot, self.disposition)
    }
}

/// Read-only facts about an X11 server relevant to running nobox.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct X11Diagnostics {
    /// Vendor string reported by the server.
    pub vendor: String,
    /// Core X protocol version.
    pub protocol_version: (u16, u16),
    /// Vendor-specific X server release number.
    pub release_number: u32,
    /// Selected screen index.
    pub screen_index: usize,
    /// Root width in pixels.
    pub width: u16,
    /// Root height in pixels.
    pub height: u16,
    /// Root drawable depth.
    pub depth: u8,
    /// Available RandR protocol version, when present.
    pub randr_version: Option<(u32, u32)>,
    /// Available Shape protocol version, when present.
    pub shape_version: Option<(u16, u16)>,
    /// Available Sync protocol version, when present.
    pub sync_version: Option<(u8, u8)>,
    /// Connected output topology, or the root fallback without RandR 1.2.
    pub outputs: Vec<Output>,
    /// Whether another window manager currently owns the ICCCM screen selection.
    pub window_manager_owner: Option<Window>,
    /// Whether the configured X11 core font can be opened.
    pub configured_font_available: bool,
    /// Whether the `fixed` startup fallback font can be opened.
    pub fallback_font_available: bool,
}

impl X11Diagnostics {
    /// Inspects an X display without selecting events or claiming WM ownership.
    ///
    /// # Errors
    ///
    /// Returns an error when the display cannot be reached or its setup and
    /// extension replies cannot be read safely.
    pub fn inspect(display: Option<&str>, configured_font: &str) -> Result<Self, X11Error> {
        let (connection, screen_index) = x11rb::connect(display)?;
        let setup = connection.setup();
        let screen = setup
            .roots
            .get(screen_index)
            .ok_or(X11Error::InvalidScreen(screen_index))?;
        let root_geometry = Geometry::new(
            0,
            0,
            u32::from(screen.width_in_pixels),
            u32::from(screen.height_in_pixels),
        );
        let randr_version = query_randr_version(&connection)?;
        let shape_version = query_shape_version(&connection)?;
        let sync_version = query_sync_version(&connection)?;
        let outputs = discover_outputs(&connection, screen.root, root_geometry, randr_version)?;
        let selection_name = format!("WM_S{screen_index}");
        let wm_selection = connection
            .intern_atom(true, selection_name.as_bytes())?
            .reply()?
            .atom;
        let owner = if wm_selection == NONE {
            NONE
        } else {
            connection.get_selection_owner(wm_selection)?.reply()?.owner
        };
        let configured_font_available = diagnostic_font_available(&connection, configured_font)?;
        let fallback_font_available = if configured_font == FALLBACK_TITLE_FONT {
            configured_font_available
        } else {
            diagnostic_font_available(&connection, FALLBACK_TITLE_FONT)?
        };
        Ok(Self {
            vendor: String::from_utf8_lossy(&setup.vendor).into_owned(),
            protocol_version: (setup.protocol_major_version, setup.protocol_minor_version),
            release_number: setup.release_number,
            screen_index,
            width: screen.width_in_pixels,
            height: screen.height_in_pixels,
            depth: screen.root_depth,
            randr_version,
            shape_version,
            sync_version,
            outputs: outputs.outputs().to_vec(),
            window_manager_owner: (owner != NONE).then_some(owner),
            configured_font_available,
            fallback_font_available,
        })
    }
}

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
    /// Connects to the nobox instance currently managing an X11 display.
    ///
    /// The EWMH supporting-window chain and manager name are verified before
    /// the private control atom is used, so a different window manager cannot
    /// receive nobox-specific requests accidentally.
    ///
    /// # Errors
    ///
    /// Returns an error when the display is unavailable or no live nobox
    /// manager publishes a valid supporting window.
    pub fn for_running_manager(display: Option<&str>) -> Result<Self, X11Error> {
        let (connection, screen_index) = x11rb::connect(display)?;
        let root = connection
            .setup()
            .roots
            .get(screen_index)
            .ok_or(X11Error::InvalidScreen(screen_index))?
            .root;
        let supporting_atom = connection
            .intern_atom(false, b"_NET_SUPPORTING_WM_CHECK")?
            .reply()?
            .atom;
        let wm_name_atom = connection
            .intern_atom(false, b"_NET_WM_NAME")?
            .reply()?
            .atom;
        let utf8_atom = connection.intern_atom(false, b"UTF8_STRING")?.reply()?.atom;
        let supporting = connection
            .get_property(false, root, supporting_atom, AtomEnum::WINDOW, 0, 1)?
            .reply()?;
        let window = supporting
            .value32()
            .and_then(|mut values| values.next())
            .filter(|window| *window != NONE)
            .ok_or(X11Error::NoRunningManager)?;
        let self_check = connection
            .get_property(false, window, supporting_atom, AtomEnum::WINDOW, 0, 1)?
            .reply()?;
        if self_check.value32().and_then(|mut values| values.next()) != Some(window) {
            return Err(X11Error::NoRunningManager);
        }
        let name = connection
            .get_property(false, window, wm_name_atom, utf8_atom, 0, 16)?
            .reply()?;
        if name.value != b"nobox" {
            return Err(X11Error::NoRunningManager);
        }
        let atom = connection
            .intern_atom(true, b"_NOBOX_CONTROL")?
            .reply()?
            .atom;
        if atom == NONE {
            return Err(X11Error::NoRunningManager);
        }
        Ok(Self {
            connection,
            window,
            atom,
        })
    }

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

    /// Requests an in-place snapshot for an external session coordinator.
    ///
    /// # Errors
    ///
    /// Returns an error when the control event cannot be delivered.
    pub fn save_session(&self) -> Result<(), X11Error> {
        self.send(CONTROL_SESSION_SAVE)
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
    SessionSave,
    KeyChainTimeout(u32),
    PingTimeout { client: ClientId, generation: u32 },
    SyncResizeTimeout { client: ClientId, generation: u32 },
    StartupTimeout(u32),
    AgentTraffic,
    AgentMarkerTimeout,
    AgentObservationTimeout(u32),
    AgentSemanticReady(u32),
    AgentSemanticTimeout(u32),
    AgentText(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActionFlow {
    Continue,
    Stop,
}

enum RuntimeTimerCommand {
    ArmAgentMarker {
        timeout: Duration,
    },
    ArmAgentObservation {
        generation: u32,
        timeout: Duration,
    },
    CancelAgentObservation(u32),
    ArmAgentSemantic {
        generation: u32,
        timeout: Duration,
    },
    CancelAgentSemantic(u32),
    ArmAgentText {
        generation: u32,
        timeout: Duration,
    },
    CancelAgentText(u32),
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
    ArmStartup {
        generation: u32,
        timeout: Duration,
    },
    Stop,
}

struct RuntimeTimer {
    commands: Sender<RuntimeTimerCommand>,
    thread: Option<JoinHandle<()>>,
}

enum ProcessReaperCommand {
    Watch(Child),
    Stop,
}

struct ProcessReaper {
    commands: Sender<ProcessReaperCommand>,
    thread: Option<JoinHandle<()>>,
}

impl ProcessReaper {
    fn spawn() -> Result<Self, X11Error> {
        let (commands, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("nobox-process-reaper".to_owned())
            .stack_size(64 * 1024)
            .spawn(move || {
                let mut children: Vec<Child> = Vec::new();
                loop {
                    children.retain_mut(|child| match child.try_wait() {
                        Ok(Some(_)) => false,
                        Ok(None) => true,
                        Err(error) => {
                            warn!(pid = child.id(), %error, "could not inspect child process");
                            false
                        }
                    });
                    let command = if children.is_empty() {
                        receiver.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    } else {
                        receiver.recv_timeout(PROCESS_REAP_INTERVAL)
                    };
                    match command {
                        Ok(ProcessReaperCommand::Watch(child)) => children.push(child),
                        Ok(ProcessReaperCommand::Stop) | Err(RecvTimeoutError::Disconnected) => {
                            break;
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .map_err(X11Error::ProcessReaperThread)?;
        Ok(Self {
            commands,
            thread: Some(thread),
        })
    }

    fn watch(&self, child: Child) -> Result<(), X11Error> {
        self.commands
            .send(ProcessReaperCommand::Watch(child))
            .map_err(|_| X11Error::ProcessReaperChannel)
    }

    fn stop(&mut self) {
        let _ = self.commands.send(ProcessReaperCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ProcessReaper {
    fn drop(&mut self) {
        self.stop();
    }
}

fn next_runtime_deadline(
    key_chain: Option<(u32, Instant)>,
    agent_deadlines: [Option<Instant>; 2],
    agent_observations: &BTreeMap<u32, Instant>,
    agent_semantics: &BTreeMap<u32, Instant>,
    pings: &BTreeMap<ClientId, (u32, Instant)>,
    sync_resize: Option<(ClientId, u32, Instant)>,
    startups: &BTreeMap<u32, Instant>,
) -> Option<Instant> {
    key_chain
        .map(|(_, deadline)| deadline)
        .into_iter()
        .chain(agent_deadlines.into_iter().flatten())
        .chain(agent_observations.values().copied())
        .chain(agent_semantics.values().copied())
        .chain(pings.values().map(|(_, deadline)| *deadline))
        .chain(sync_resize.map(|(_, _, deadline)| deadline))
        .chain(startups.values().copied())
        .min()
}

impl RuntimeTimer {
    fn spawn(control: ControlSender) -> Result<Self, X11Error> {
        let (commands, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("nobox-runtime-timer".to_owned())
            .stack_size(64 * 1024)
            .spawn(move || {
                let mut key_chain = None;
                let mut agent_marker: Option<Instant> = None;
                let mut agent_observations: BTreeMap<u32, Instant> = BTreeMap::new();
                let mut agent_semantics: BTreeMap<u32, Instant> = BTreeMap::new();
                let mut agent_text: Option<(u32, Instant)> = None;
                let mut pings: BTreeMap<ClientId, (u32, Instant)> = BTreeMap::new();
                let mut sync_resize = None;
                let mut startups: BTreeMap<u32, Instant> = BTreeMap::new();
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
                    if let Some(deadline) = agent_marker
                        && deadline <= now
                    {
                        agent_marker = None;
                        if let Err(error) = control.send_data(CONTROL_AGENT_MARKER, 0) {
                            warn!(%error, "could not deliver the agent marker timeout");
                            break;
                        }
                    }
                    let mut delivery_failed = false;
                    while let Some(generation) =
                        agent_observations
                            .iter()
                            .find_map(|(generation, deadline)| {
                                (*deadline <= now).then_some(*generation)
                            })
                    {
                        agent_observations.remove(&generation);
                        if let Err(error) = control.send_data(CONTROL_AGENT_OBSERVATION, generation)
                        {
                            warn!(%error, "could not deliver an agent observation timeout");
                            delivery_failed = true;
                            break;
                        }
                    }
                    if delivery_failed {
                        break;
                    }
                    while let Some(generation) =
                        agent_semantics.iter().find_map(|(generation, deadline)| {
                            (*deadline <= now).then_some(*generation)
                        })
                    {
                        agent_semantics.remove(&generation);
                        if let Err(error) =
                            control.send_data(CONTROL_AGENT_SEMANTIC_TIMEOUT, generation)
                        {
                            warn!(%error, "could not deliver a semantic reply timeout");
                            delivery_failed = true;
                            break;
                        }
                    }
                    if delivery_failed {
                        break;
                    }
                    if let Some((generation, deadline)) = agent_text
                        && deadline <= now
                    {
                        agent_text = None;
                        if let Err(error) = control.send_data(CONTROL_AGENT_TEXT, generation) {
                            warn!(%error, "could not deliver paced agent-text tick");
                            break;
                        }
                    }
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
                    while let Some(generation) =
                        startups.iter().find_map(|(generation, deadline)| {
                            (*deadline <= now).then_some(*generation)
                        })
                    {
                        startups.remove(&generation);
                        if let Err(error) = control.send_data(CONTROL_STARTUP_TIMEOUT, generation) {
                            warn!(%error, "could not deliver startup-notification timeout");
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

                    let deadline = next_runtime_deadline(
                        key_chain,
                        [agent_marker, agent_text.map(|(_, deadline)| deadline)],
                        &agent_observations,
                        &agent_semantics,
                        &pings,
                        sync_resize,
                        &startups,
                    );
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
                        RuntimeTimerCommand::ArmAgentMarker { timeout } => {
                            agent_marker = Some(Instant::now() + timeout);
                        }
                        RuntimeTimerCommand::ArmAgentObservation {
                            generation,
                            timeout,
                        } => {
                            agent_observations.insert(generation, Instant::now() + timeout);
                        }
                        RuntimeTimerCommand::CancelAgentObservation(generation) => {
                            agent_observations.remove(&generation);
                        }
                        RuntimeTimerCommand::ArmAgentSemantic {
                            generation,
                            timeout,
                        } => {
                            agent_semantics.insert(generation, Instant::now() + timeout);
                        }
                        RuntimeTimerCommand::CancelAgentSemantic(generation) => {
                            agent_semantics.remove(&generation);
                        }
                        RuntimeTimerCommand::ArmAgentText {
                            generation,
                            timeout,
                        } => agent_text = Some((generation, Instant::now() + timeout)),
                        RuntimeTimerCommand::CancelAgentText(generation) => {
                            if agent_text.is_some_and(|(current, _)| current == generation) {
                                agent_text = None;
                            }
                        }
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
                        RuntimeTimerCommand::ArmStartup {
                            generation,
                            timeout,
                        } => {
                            startups.insert(generation, Instant::now() + timeout);
                        }
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

    fn arm_agent_marker(&self, timeout: Duration) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::ArmAgentMarker { timeout })
            .map_err(|_| X11Error::TimerChannel)
    }

    fn arm_agent_observation(&self, generation: u32, timeout: Duration) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::ArmAgentObservation {
                generation,
                timeout,
            })
            .map_err(|_| X11Error::TimerChannel)
    }

    fn cancel_agent_observation(&self, generation: u32) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::CancelAgentObservation(generation))
            .map_err(|_| X11Error::TimerChannel)
    }

    fn arm_agent_semantic(&self, generation: u32, timeout: Duration) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::ArmAgentSemantic {
                generation,
                timeout,
            })
            .map_err(|_| X11Error::TimerChannel)
    }

    fn cancel_agent_semantic(&self, generation: u32) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::CancelAgentSemantic(generation))
            .map_err(|_| X11Error::TimerChannel)
    }

    fn arm_agent_text(&self, generation: u32, timeout: Duration) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::ArmAgentText {
                generation,
                timeout,
            })
            .map_err(|_| X11Error::TimerChannel)
    }

    fn cancel_agent_text(&self, generation: u32) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::CancelAgentText(generation))
            .map_err(|_| X11Error::TimerChannel)
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

    fn arm_startup(&self, generation: u32, timeout: Duration) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::ArmStartup {
                generation,
                timeout,
            })
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

struct AgentSeatOwnership {
    window: Window,
    timestamp: u32,
    advertisement: Vec<u8>,
}

/// A running connection that owns the X11 window-manager selection.
pub struct WindowManager {
    connection: RustConnection,
    screen_index: usize,
    root: Window,
    support_window: Window,
    wm_selection: u32,
    wm_selection_timestamp: u32,
    agent_selection: u32,
    desktop_layout_selection: u32,
    atoms: Atoms,
    config: Config,
    application_catalog: ApplicationCatalog,
    clients: ClientSet,
    application_identities: BTreeMap<ClientId, X11ApplicationIdentity>,
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
    frame_sync: std::cell::RefCell<BTreeMap<ClientId, FrameSyncState>>,
    published_client_list: std::cell::RefCell<Option<Vec<u32>>>,
    published_client_stacking: std::cell::RefCell<Option<Vec<u32>>>,
    frame_parts: BTreeMap<Window, FramePart>,
    hovered_frame_button: Option<Window>,
    pressed_frame_button: Option<Window>,
    decoration_pixels: DecorationPixels,
    cursors: CursorPalette,
    title_font: TitleFont,
    title_gc: Gcontext,
    focus_indicator: FocusIndicator,
    focus_overlay: FocusOverlay,
    menu_overlay: MenuOverlay,
    menu_session: Option<MenuSession>,
    menu_keycodes: MenuKeycodes,
    key_bindings: KeyBindingNode,
    chain_quit_bindings: Vec<KeyInput>,
    key_chain: Option<KeyChain>,
    key_chain_generation: u32,
    runtime_timer: RuntimeTimer,
    semantic_runner: Option<semantic::Runner>,
    process_reaper: ProcessReaper,
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
    pending_new_focus: Option<ClientId>,
    expected_unmaps: BTreeMap<Window, u8>,
    explicit_desktop_clients: BTreeSet<ClientId>,
    startup_sequences: BTreeMap<String, StartupSequence>,
    startup_message_buffers: BTreeMap<Window, Vec<u8>>,
    startup_generation: u32,
    show_desktop_strict: bool,
    last_timestamp: u32,
    last_user_time: u32,
    running: bool,
    session_logout_requested: bool,
    disposition: RunDisposition,
    agent_seat: Option<agent::AgentSeat>,
    agent_seat_ownership: Option<AgentSeatOwnership>,
    agent_state: AgentState,
    agent_scopes: BTreeMap<AgentSessionId, ApplicationMatcher>,
    agent_shadow: BTreeMap<ClientId, AgentShadow>,
    agent_injections: VecDeque<InjectedInput>,
    agent_input_target: Option<(ClientId, Instant)>,
    agent_indicator: Option<Window>,
    agent_kill_chord: Vec<KeyInput>,
    agent_consented: BTreeSet<AgentSessionId>,
    agent_display: Option<String>,
    agent_consent: Option<ActiveConsent>,
    agent_consent_queue: VecDeque<PendingConsent>,
    raw_input_selected: bool,
    composite_version: Option<(u32, u32)>,
    keyboard_layout: Option<KeyboardLayout>,
    last_human_input: Option<Instant>,
    last_human_event: Option<Instant>,
    agent_launch_tokens: BTreeMap<ClientId, String>,
    agent_launch_pending: BTreeSet<String>,
    agent_observations: BTreeMap<u32, PendingAgentObservation>,
    agent_observation_generation: u32,
    agent_text: Option<PendingAgentText>,
    agent_text_generation: u32,
    agent_semantics: BTreeMap<u32, PendingAgentSemantic>,
    agent_semantic_generation: u32,
    agent_semantic_trees: BTreeMap<(AgentSessionId, ClientId), AgentSemanticTree>,
    agent_focus: Option<ClientId>,
    agent_workspace: WorkspaceId,
    deferred_events: VecDeque<Event>,
}

/// A handshake waiting for a person to answer it.
#[derive(Clone, Debug)]
struct PendingConsent {
    session: AgentSessionId,
    hello: nobox_agent_wire::Hello,
    uid: u32,
    pid: i32,
    executable: Option<PathBuf>,
}

/// The consent dialog currently on screen.
#[derive(Clone, Debug)]
struct ActiveConsent {
    pending: PendingConsent,
    window: Window,
    lines: Vec<String>,
}

#[derive(Clone, Debug)]
struct PendingAgentObservation {
    generation: u32,
    session: AgentSessionId,
    request: AgentRequestId,
    tool: &'static str,
    action: AgentActionId,
    target: ClientId,
    capture: Option<nobox_agent_wire::ObservationCapture>,
    committed: Vec<AgentStep>,
    started: Instant,
    started_sequence: nobox_agent_wire::Sequence,
    minimum: Duration,
    quiet: Duration,
    maximum: Duration,
    last_event: Instant,
    events: Vec<nobox_agent_wire::EventEnvelope>,
    dropped_events: u64,
}

#[derive(Clone, Debug)]
struct PendingAgentText {
    generation: u32,
    session: AgentSessionId,
    request: AgentRequestId,
    tool: &'static str,
    call: nobox_agent_wire::Call,
    target: ClientId,
    plan: PendingAgentTextPlan,
    committed: Vec<AgentStep>,
    action: AgentActionId,
    observe: Option<nobox_agent_wire::ObservationRequest>,
}

#[derive(Clone, Debug)]
enum PendingAgentTextPlan {
    Strokes(VecDeque<AgentTextStroke>),
    TransferPending {
        text: String,
        client_base: u32,
        paste: AgentPasteChord,
    },
    TransferOffered {
        text: String,
        client_base: u32,
        deadline: Instant,
        last_delivery: Option<Instant>,
    },
}

#[derive(Clone, Debug)]
struct PendingAgentSemantic {
    generation: u32,
    session: AgentSessionId,
    request: AgentRequestId,
    tool: &'static str,
    call: nobox_agent_wire::Call,
    target: ClientId,
    client_generation: nobox_agent_wire::Generation,
    pid: u32,
    cancelled: bool,
    result: Option<semantic::Result>,
    projection: Option<PendingSemanticProjection>,
    search: Option<PendingSemanticSearch>,
}

#[derive(Clone, Copy, Debug)]
struct PendingSemanticProjection {
    tree_generation: nobox_agent_wire::TreeGeneration,
    root: u64,
    offset: u16,
    max_nodes: u16,
    max_depth: u8,
    source_continuation: Option<nobox_agent_wire::SemanticContinuation>,
}

#[derive(Clone, Debug)]
struct PendingSemanticSearch {
    tree_generation: nobox_agent_wire::TreeGeneration,
    offset: u16,
    max_results: u16,
    query: nobox_agent_wire::SemanticQuery,
    source_continuation: Option<nobox_agent_wire::SemanticContinuation>,
}

#[derive(Clone)]
enum AgentSemanticCursor {
    Tree {
        root: u64,
        offset: u16,
        max_depth: u8,
    },
    Search {
        offset: u16,
        query: nobox_agent_wire::SemanticQuery,
    },
}

struct AgentSemanticTree {
    generation: nobox_agent_wire::TreeGeneration,
    root: u64,
    public_by_internal: BTreeMap<u64, nobox_agent_wire::SemanticNodeId>,
    internal_by_public: BTreeMap<nobox_agent_wire::SemanticNodeId, u64>,
    next_node: u64,
    continuations: BTreeMap<nobox_agent_wire::SemanticContinuation, AgentSemanticCursor>,
    next_continuation: u64,
}

impl AgentSemanticTree {
    fn new(generation: nobox_agent_wire::TreeGeneration, root: u64) -> Self {
        let root_id = nobox_agent_wire::SemanticNodeId::new(1);
        Self {
            generation,
            root,
            public_by_internal: BTreeMap::from([(root, root_id)]),
            internal_by_public: BTreeMap::from([(root_id, root)]),
            next_node: 2,
            continuations: BTreeMap::new(),
            next_continuation: 1,
        }
    }

    fn public_id(&mut self, internal: u64) -> nobox_agent_wire::SemanticNodeId {
        if let Some(id) = self.public_by_internal.get(&internal) {
            return *id;
        }
        let id = nobox_agent_wire::SemanticNodeId::new(self.next_node);
        self.next_node = self.next_node.saturating_add(1);
        self.public_by_internal.insert(internal, id);
        self.internal_by_public.insert(id, internal);
        id
    }

    fn issue_continuation(
        &mut self,
        cursor: AgentSemanticCursor,
    ) -> nobox_agent_wire::SemanticContinuation {
        if self.next_continuation == u64::MAX {
            self.continuations.clear();
            self.next_continuation = 1;
        }
        if self.continuations.len() >= MAX_SEMANTIC_CONTINUATIONS
            && let Some(oldest) = self.continuations.keys().next().copied()
        {
            self.continuations.remove(&oldest);
        }
        let continuation = nobox_agent_wire::SemanticContinuation::new(self.next_continuation);
        self.next_continuation = self.next_continuation.saturating_add(1);
        self.continuations.insert(continuation, cursor);
        continuation
    }
}

fn valid_semantic_projection(
    projection: PendingSemanticProjection,
    matched: &semantic::Match,
) -> bool {
    if matched.nodes.is_empty() || matched.nodes.len() > usize::from(projection.max_nodes) {
        return false;
    }
    let Some(returned) = u16::try_from(matched.nodes.len()).ok() else {
        return false;
    };
    let Some(expected_next) = projection.offset.checked_add(returned) else {
        return false;
    };
    if expected_next > nobox_agent_wire::MAX_SEMANTIC_SCAN_NODES {
        return false;
    }
    if matched
        .next_offset
        .is_some_and(|offset| offset != expected_next || returned != projection.max_nodes)
    {
        return false;
    }
    if projection.offset == 0
        && !matches!(
            matched.nodes.first(),
            Some(node) if node.id == projection.root && node.parent.is_none() && node.depth == 0
        )
    {
        return false;
    }
    let mut depths = BTreeMap::<u64, u8>::new();
    matched.nodes.iter().enumerate().all(|(index, node)| {
        if node.depth > projection.max_depth {
            return false;
        }
        if (projection.offset > 0 || index > 0) && (node.parent.is_none() || node.depth == 0) {
            return false;
        }
        if let Some(parent_depth) = node.parent.and_then(|parent| depths.get(&parent))
            && parent_depth.checked_add(1) != Some(node.depth)
        {
            return false;
        }
        depths.insert(node.id, node.depth);
        true
    })
}

fn semantic_query_matches(query: &nobox_agent_wire::SemanticQuery, node: &semantic::Node) -> bool {
    query.name.as_ref().is_none_or(|needle| {
        node.name
            .as_ref()
            .is_some_and(|name| name.to_lowercase().contains(&needle.to_lowercase()))
    }) && (query.roles.is_empty() || query.roles.contains(&node.role))
        && query.states.iter().all(|state| node.states.contains(state))
}

fn valid_semantic_search(search: &PendingSemanticSearch, matched: &semantic::Match) -> bool {
    if matched.nodes.len() > usize::from(search.max_results) {
        return false;
    }
    if matched.next_offset.is_some_and(|offset| {
        offset <= search.offset
            || offset > nobox_agent_wire::MAX_SEMANTIC_SCAN_NODES
            || matched.nodes.len() != usize::from(search.max_results)
    }) {
        return false;
    }
    matched
        .nodes
        .iter()
        .all(|node| semantic_query_matches(&search.query, node))
}

impl PendingAgentObservation {
    fn capture_client(&self) -> Option<nobox_agent_wire::ClientId> {
        self.capture.map(|capture| {
            capture
                .client
                .unwrap_or_else(|| agent_client_id(self.target))
        })
    }

    fn deadline(&self) -> Instant {
        let earliest = self.started + self.minimum;
        let quiet = self.last_event + self.quiet;
        earliest.max(quiet).min(self.started + self.maximum)
    }

    fn accepts(&self, kind: nobox_agent_wire::EventKind, subject: Option<ClientId>) -> bool {
        subject == Some(self.target)
            || matches!(
                kind,
                nobox_agent_wire::EventKind::FocusChanged
                    | nobox_agent_wire::EventKind::WorkspaceSwitched
            )
    }

    fn record(&mut self, envelope: nobox_agent_wire::EventEnvelope, now: Instant) {
        self.last_event = now;
        if self.events.len() < nobox_agent_wire::MAX_ACTION_OBSERVATION_EVENTS {
            self.events.push(envelope);
        } else {
            self.dropped_events = self.dropped_events.saturating_add(1);
        }
    }
}

enum AgentCallResult {
    Ready(AgentOutcome),
    DeferredObservation(PendingAgentObservation),
    DeferredText(PendingAgentText),
    DeferredSemantic {
        pending: Box<PendingAgentSemantic>,
        helper_request: Option<semantic::Request>,
    },
}

#[derive(Clone, Copy)]
struct AgentInputRequest<'a> {
    session: AgentSessionId,
    request: AgentRequestId,
    tool: &'static str,
    client: nobox_agent_wire::ClientId,
    expects: &'a nobox_agent_wire::Expects,
    ensure_visible: bool,
    observe: Option<nobox_agent_wire::ObservationRequest>,
}

impl From<AgentOutcome> for AgentCallResult {
    fn from(outcome: AgentOutcome) -> Self {
        Self::Ready(outcome)
    }
}

/// What a person answered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConsentAnswer {
    Deny,
    Once,
    Persist,
}

/// One input event the manager synthesized itself.
#[derive(Clone, Copy, Debug)]
struct InjectedInput {
    type_: u8,
    detail: u8,
    expires: Instant,
}

/// The keyboard mapping in force, kept so agent input can find keys.
#[derive(Clone, Debug)]
struct KeyboardLayout {
    minimum: u8,
    per_keycode: u8,
    keysyms: Vec<u32>,
}

/// One fully resolved character injection, prepared before any X event is sent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AgentTextStroke {
    keycode: u8,
    modifiers: [Option<u8>; 2],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AgentPasteChord {
    control: u8,
    key: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AgentTextPlan {
    Strokes(Vec<AgentTextStroke>),
    Transfer {
        text: String,
        client_base: u32,
        paste: AgentPasteChord,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentTextPlanError {
    Unsupported(char),
    MissingModifier {
        character: char,
        modifier: nobox_agent_wire::Modifier,
    },
}

/// The last state an agent session was told about one client.
///
/// Events are the difference between this and the live desktop, so no change
/// can be missed and none is reported twice.
#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentShadow {
    title: Option<String>,
    state: nobox_agent_wire::ClientState,
    content: nobox_agent_wire::Rect,
    frame: nobox_agent_wire::Rect,
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
        let mut deferred_events = VecDeque::new();
        let timestamp = server_timestamp(
            &connection,
            support_window,
            atoms._NOBOX_TIMESTAMP,
            &mut deferred_events,
        )?;

        let claim = connection.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(root_events()),
        )?;
        if let Err(error) = claim.check() {
            return Err(X11Error::RootClaim(error));
        }
        let cursors = CursorPalette::load(&connection)?;
        connection
            .change_window_attributes(
                root,
                &ChangeWindowAttributesAux::new().cursor(cursors.pointer),
            )?
            .check()?;

        let selection_name = format!("WM_S{screen_index}");
        let wm_selection = connection
            .intern_atom(false, selection_name.as_bytes())?
            .reply()?
            .atom;
        let agent_selection_name = format!("_AGENT_SEAT_S{screen_index}");
        let agent_selection = connection
            .intern_atom(false, agent_selection_name.as_bytes())?
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
        let title_font = load_title_font_with_fallback(&connection, &config.theme.font)?;
        let title_gc = connection.generate_id()?;
        connection
            .create_gc(
                title_gc,
                root,
                &CreateGCAux::new()
                    .font(title_font.id)
                    .foreground(decoration_pixels.title_text),
            )?
            .check()?;
        let runtime_timer = RuntimeTimer::spawn(ControlSender::connect(
            display,
            support_window,
            atoms._NOBOX_CONTROL,
        )?)?;
        let semantic_runner = if config.agent.enabled {
            match ControlSender::connect(display, support_window, atoms._NOBOX_CONTROL) {
                Ok(control) => match semantic::Runner::spawn(control) {
                    Ok(runner) => Some(runner),
                    Err(error) => {
                        warn!(%error, "semantic helper runner is unavailable");
                        None
                    }
                },
                Err(error) => {
                    warn!(%error, "semantic helper wakeup connection is unavailable");
                    None
                }
            }
        } else {
            None
        };
        let process_reaper = ProcessReaper::spawn()?;

        let create_focus_indicator_window = |name: &[u8]| -> Result<Window, X11Error> {
            let window = connection.generate_id()?;
            connection
                .create_window(
                    COPY_DEPTH_FROM_PARENT,
                    window,
                    root,
                    0,
                    0,
                    1,
                    1,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new()
                        .background_pixel(decoration_pixels.active_border)
                        .cursor(cursors.pointer)
                        .override_redirect(1_u32)
                        .save_under(1_u32),
                )?
                .check()?;
            connection.change_property8(
                x11rb::protocol::xproto::PropMode::REPLACE,
                window,
                atoms._NET_WM_NAME,
                atoms.UTF8_STRING,
                name,
            )?;
            connection.change_property8(
                x11rb::protocol::xproto::PropMode::REPLACE,
                window,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                b"nobox-focus-indicator\0nobox\0",
            )?;
            Ok(window)
        };
        let focus_indicator_windows = [
            create_focus_indicator_window(b"nobox:focus-indicator-top")?,
            create_focus_indicator_window(b"nobox:focus-indicator-left")?,
            create_focus_indicator_window(b"nobox:focus-indicator-right")?,
            create_focus_indicator_window(b"nobox:focus-indicator-bottom")?,
        ];

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
                    .cursor(cursors.pointer)
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
                    .cursor(cursors.pointer)
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
        } else {
            clients.switch_workspace(WorkspaceId::new(config.workspaces.initial - 1));
        }
        let work_areas =
            vec![screen_geometry; usize::try_from(clients.workspace_count()).unwrap_or(1)];
        let mut output_work_areas = BTreeMap::new();
        for output in outputs.outputs() {
            for workspace in 0..clients.workspace_count() {
                output_work_areas.insert((output.id, WorkspaceId::new(workspace)), output.geometry);
            }
        }
        let application_catalog = ApplicationCatalog::discover();
        let mut wm = Self {
            connection,
            screen_index,
            root,
            support_window,
            wm_selection,
            wm_selection_timestamp: timestamp,
            agent_selection,
            desktop_layout_selection,
            atoms,
            config,
            application_catalog,
            clients,
            application_identities: BTreeMap::new(),
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
            frame_sync: std::cell::RefCell::new(BTreeMap::new()),
            published_client_list: std::cell::RefCell::new(None),
            published_client_stacking: std::cell::RefCell::new(None),
            frame_parts: BTreeMap::new(),
            hovered_frame_button: None,
            pressed_frame_button: None,
            decoration_pixels,
            cursors,
            title_font,
            title_gc,
            focus_indicator: FocusIndicator {
                windows: focus_indicator_windows,
                mapped: false,
            },
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
            semantic_runner,
            process_reaper,
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
            pending_new_focus: None,
            expected_unmaps: BTreeMap::new(),
            explicit_desktop_clients: BTreeSet::new(),
            startup_sequences: BTreeMap::new(),
            startup_message_buffers: BTreeMap::new(),
            startup_generation: 0,
            show_desktop_strict: false,
            last_timestamp: timestamp,
            last_user_time: CURRENT_TIME,
            running: true,
            session_logout_requested: false,
            disposition: RunDisposition::Exit,
            agent_seat: None,
            agent_seat_ownership: None,
            agent_state: AgentState::new(),
            agent_scopes: BTreeMap::new(),
            agent_shadow: BTreeMap::new(),
            agent_injections: VecDeque::new(),
            agent_input_target: None,
            agent_indicator: None,
            agent_kill_chord: Vec::new(),
            agent_consented: BTreeSet::new(),
            agent_display: display.map(ToOwned::to_owned),
            agent_consent: None,
            agent_consent_queue: VecDeque::new(),
            raw_input_selected: false,
            composite_version: None,
            keyboard_layout: None,
            last_human_input: None,
            last_human_event: None,
            agent_launch_tokens: BTreeMap::new(),
            agent_launch_pending: BTreeSet::new(),
            agent_observations: BTreeMap::new(),
            agent_observation_generation: 0,
            agent_text: None,
            agent_text_generation: 0,
            agent_semantics: BTreeMap::new(),
            agent_semantic_generation: 0,
            agent_semantic_trees: BTreeMap::new(),
            agent_focus: None,
            agent_workspace: WorkspaceId::new(0),
            deferred_events,
        };
        wm.refresh_workspace_layout()?;
        wm.refresh_work_area()?;
        wm.publish_identity()?;
        if let Err(error) = wm.start_agent_seat() {
            warn!(%error, "could not start the agent seat");
        }
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
        self,
        load_config: impl FnMut() -> Result<Config, E>,
    ) -> Result<RunOutcome, X11Error>
    where
        E: std::fmt::Display,
    {
        self.run_with_session_save(load_config, |_| true)
    }

    /// Processes events while allowing an external coordinator to save state.
    ///
    /// The save callback runs only after an explicit control request and must
    /// return promptly. Its boolean result is logged and can be returned to the
    /// process-level coordinator.
    ///
    /// # Errors
    ///
    /// Returns an error when communication with the X server fails.
    pub fn run_with_session_save<E>(
        self,
        load_config: impl FnMut() -> Result<Config, E>,
        save_session: impl FnMut(&SessionSnapshot) -> bool,
    ) -> Result<RunOutcome, X11Error>
    where
        E: std::fmt::Display,
    {
        self.run_with_session_coordination(load_config, save_session, || false)
    }

    /// Processes events with save and global-logout session coordination.
    ///
    /// A confirmed session logout invokes `request_logout` at a coherent event
    /// boundary. Returning `true` leaves the manager running until the external
    /// coordinator sends cancellation or `Die`; returning `false` falls back to
    /// a clean local window-manager exit.
    ///
    /// # Errors
    ///
    /// Returns an error when communication with the X server fails.
    pub fn run_with_session_coordination<E>(
        mut self,
        mut load_config: impl FnMut() -> Result<Config, E>,
        mut save_session: impl FnMut(&SessionSnapshot) -> bool,
        mut request_logout: impl FnMut() -> bool,
    ) -> Result<RunOutcome, X11Error>
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
        let mut pending_focus_events = 0_u16;
        while self.running {
            let mut event = if let Some(event) = self.deferred_events.pop_front() {
                event
            } else {
                self.connection.wait_for_event()?
            };
            // Coalesce a backlog of pointer-motion reports for the same window
            // and button state: only the newest position matters for drags and
            // hover tracking, and processing stale ones only adds latency.
            while matches!(&event, Event::MotionNotify(_)) {
                let Some(next) = self.connection.poll_for_event()? else {
                    break;
                };
                let same_motion_stream = match (&event, &next) {
                    (Event::MotionNotify(current), Event::MotionNotify(newer)) => {
                        newer.event == current.event
                            && newer.child == current.child
                            && newer.state == current.state
                            && newer.same_screen == current.same_screen
                    }
                    _ => false,
                };
                if same_motion_stream {
                    event = next;
                } else {
                    self.deferred_events.push_front(next);
                    break;
                }
            }
            let direct_user_input = match &event {
                Event::ButtonPress(event) => event.response_type & 0x80 == 0,
                Event::KeyPress(event) => event.response_type & 0x80 == 0,
                _ => false,
            };
            self.note_input_provenance(&event);
            if direct_user_input {
                self.pending_new_focus = None;
            }
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
                Some(RuntimeRequest::AgentTraffic) => self.drain_agent_traffic(),
                Some(RuntimeRequest::AgentMarkerTimeout) => {
                    if let Err(error) = self.expire_agent_input_target() {
                        warn!(%error, "could not clear the agent input marker");
                    }
                }
                Some(RuntimeRequest::AgentObservationTimeout(generation)) => {
                    self.finish_agent_observation(generation);
                }
                Some(RuntimeRequest::AgentSemanticReady(_)) => {
                    self.collect_agent_semantic_results();
                }
                Some(RuntimeRequest::AgentSemanticTimeout(generation)) => {
                    self.finish_agent_semantic(generation);
                }
                Some(RuntimeRequest::AgentText(generation)) => {
                    self.advance_agent_text(generation);
                }
                Some(RuntimeRequest::SessionSave) => {
                    if save_session(&self.session_snapshot()) {
                        info!("external session snapshot completed");
                    } else {
                        warn!("external session snapshot failed");
                    }
                }
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
                Some(RuntimeRequest::StartupTimeout(generation)) => {
                    let startup_id =
                        self.startup_sequences
                            .iter()
                            .find_map(|(startup_id, sequence)| {
                                (sequence.generation == generation).then(|| startup_id.clone())
                            });
                    if let Some(startup_id) = startup_id {
                        self.complete_startup_notification(&startup_id)?;
                    }
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
            if self.running && std::mem::take(&mut self.session_logout_requested) {
                if request_logout() {
                    info!("requested logout from the external session manager");
                } else {
                    info!("no external session manager accepted logout; exiting nobox cleanly");
                    self.running = false;
                    self.disposition = RunDisposition::Exit;
                }
            }
            if self.deferred_events.is_empty()
                && let Some(event) = self.connection.poll_for_event()?
            {
                self.deferred_events.push_back(event);
            }
            pending_focus_events = if self.pending_new_focus.is_some() {
                pending_focus_events.saturating_add(1)
            } else {
                0
            };
            if self.deferred_events.is_empty() || pending_focus_events >= 256 {
                self.finish_pending_new_focus()?;
                pending_focus_events = 0;
            }
            if self.deferred_events.is_empty() {
                self.sync_agent_events();
                self.flush_agent_events();
            }
            // Requests only need to reach the server before the loop blocks in
            // wait_for_event; a burst of queued events flushes once at its end.
            if self.deferred_events.is_empty() {
                self.connection.flush()?;
            }
        }
        self.stop_agent_seat();
        self.connection.flush()?;
        let snapshot = self.session_snapshot();
        info!("nobox X11 event loop stopped cleanly");
        Ok(RunOutcome {
            snapshot,
            disposition: self.disposition.clone(),
        })
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

    /// Publishes everything that changed since the last boundary.
    ///
    /// The event stream is produced by comparing the live desktop against a
    /// shadow copy rather than by hooking every mutation site. Nothing can
    /// change without appearing here, which is what makes the stream worth
    /// maintaining a world model against, and it coalesces by construction:
    /// an interactive drag moves geometry many times and emits the settled
    /// result once.
    fn sync_agent_events(&mut self) {
        if self.agent_seat.is_none() {
            return;
        }
        for client in self.agent_shadow.keys().copied().collect::<Vec<_>>() {
            if !self.clients.contains(client) {
                self.agent_shadow.remove(&client);
                let identity = agent_client_id(client);
                self.emit_agent_event(
                    nobox_agent_wire::EventKind::ClientClosed,
                    Some(client),
                    |_, _| Some(nobox_agent_wire::Event::ClientClosed { client: identity }),
                );
                self.forget_agent_client(client);
            }
        }
        let settled = self.drag.is_none();
        for client in self.clients.management_order().collect::<Vec<_>>() {
            self.sync_agent_client(client, settled);
        }
        let focused = self.clients.focused();
        if self.agent_focus != focused {
            self.agent_focus = focused;
            self.emit_agent_event(
                nobox_agent_wire::EventKind::FocusChanged,
                None,
                |manager, session| {
                    // A session that cannot perceive the newly focused client
                    // is told focus left everything it can see, never who
                    // took it.
                    let visible =
                        focused.filter(|client| manager.agent_state.perceives(session, *client));
                    Some(nobox_agent_wire::Event::FocusChanged {
                        client: visible.map(agent_client_id),
                    })
                },
            );
        }
        let workspace = self.clients.current_workspace();
        if self.agent_workspace != workspace {
            self.agent_workspace = workspace;
            self.emit_agent_event(
                nobox_agent_wire::EventKind::WorkspaceSwitched,
                None,
                |_, _| {
                    Some(nobox_agent_wire::Event::WorkspaceSwitched {
                        workspace: nobox_agent_wire::WorkspaceId::new(workspace.index()),
                    })
                },
            );
        }
    }

    fn sync_agent_client(&mut self, client: ClientId, settled: bool) {
        let Some(state) = nobox_core::agent::client_state(&self.clients, client) else {
            return;
        };
        let content = agent_rect(
            self.clients
                .get(client)
                .map_or_else(|| Geometry::new(0, 0, 1, 1), |managed| managed.geometry),
        );
        let frame = agent_rect(AgentClientDetails::frame(self, client));
        let title = self.titles.get(&client).cloned();
        let Some(previous) = self.agent_shadow.get(&client).cloned() else {
            self.agent_shadow.insert(
                client,
                AgentShadow {
                    title,
                    state,
                    content,
                    frame,
                },
            );
            let launch = self.agent_launch_tokens.remove(&client);
            self.emit_agent_event(
                nobox_agent_wire::EventKind::ClientMapped,
                Some(client),
                |manager, session| {
                    let descriptor = manager.agent_state.descriptor(
                        session,
                        client,
                        &manager.clients,
                        &manager.outputs,
                        manager,
                    )?;
                    Some(nobox_agent_wire::Event::ClientMapped {
                        client: Box::new(descriptor),
                        launch: launch.clone(),
                    })
                },
            );
            return;
        };
        if previous.title != title {
            // A rule may match on the title, and a title can change. Rules are
            // re-evaluated so a window that renames itself into a sensitive
            // rule is hidden from here on; visibility never relaxes.
            self.register_agent_client(client);
        }
        let title_changed = previous.title != title;
        let state_changed = previous.state != state;
        let geometry_changed = settled && (previous.content != content || previous.frame != frame);
        if !title_changed && !state_changed && !geometry_changed {
            return;
        }
        if let Some(shadow) = self.agent_shadow.get_mut(&client) {
            if title_changed {
                shadow.title.clone_from(&title);
            }
            if state_changed {
                shadow.state = state;
            }
            if geometry_changed {
                shadow.content = content;
                shadow.frame = frame;
            }
        }
        if title_changed {
            let generation = self.agent_state.touch(client);
            self.emit_agent_event(
                nobox_agent_wire::EventKind::TitleChanged,
                Some(client),
                |manager, session| {
                    // Titles are their own capability, and a redacted client
                    // never has one, so the payload is per session.
                    let descriptor = manager.agent_state.descriptor(
                        session,
                        client,
                        &manager.clients,
                        &manager.outputs,
                        manager,
                    )?;
                    Some(nobox_agent_wire::Event::TitleChanged {
                        client: agent_client_id(client),
                        generation,
                        title: descriptor.title,
                    })
                },
            );
        }
        if state_changed {
            let generation = self.agent_state.touch(client);
            self.emit_agent_event(
                nobox_agent_wire::EventKind::StateChanged,
                Some(client),
                |_, _| {
                    Some(nobox_agent_wire::Event::StateChanged {
                        client: agent_client_id(client),
                        generation,
                        state,
                    })
                },
            );
        }
        if geometry_changed {
            let generation = self.agent_state.touch(client);
            self.emit_agent_event(
                nobox_agent_wire::EventKind::GeometryChanged,
                Some(client),
                |_, _| {
                    Some(nobox_agent_wire::Event::GeometryChanged {
                        client: agent_client_id(client),
                        generation,
                        content,
                        frame,
                    })
                },
            );
        }
    }

    /// Builds one event per interested session and publishes them together.
    fn emit_agent_event(
        &mut self,
        kind: nobox_agent_wire::EventKind,
        subject: Option<ClientId>,
        build: impl Fn(&Self, AgentSessionId) -> Option<nobox_agent_wire::Event>,
    ) {
        // The desktop moved for everyone who could see it, whether or not they
        // asked to be told. Advancing first, for observers rather than
        // subscribers, is what keeps a session's sequence meaningful when it
        // never subscribed — and keeps it independent of whether some other
        // session did.
        self.agent_state.touch_observers(subject);
        if let Some(subject) = subject {
            let generations = self
                .agent_semantics
                .iter()
                .filter_map(|(generation, pending)| {
                    (pending.target == subject).then_some(*generation)
                })
                .collect::<Vec<_>>();
            for generation in generations {
                self.cancel_agent_semantic(generation);
            }
        }
        let subscribers: BTreeSet<AgentSessionId> = self
            .agent_state
            .subscribers(kind, subject)
            .into_iter()
            .collect();
        let observer_sessions: BTreeSet<AgentSessionId> = self
            .agent_observations
            .values()
            .filter(|pending| pending.accepts(kind, subject))
            .map(|pending| pending.session)
            .collect();
        let targets: BTreeSet<AgentSessionId> =
            subscribers.union(&observer_sessions).copied().collect();
        if targets.is_empty() {
            return;
        }
        let events: Vec<(AgentSessionId, nobox_agent_wire::Event)> = targets
            .into_iter()
            .filter_map(|session| build(self, session).map(|event| (session, event)))
            .collect();
        let now = Instant::now();
        let mut rearm = Vec::new();
        for (session, event) in &events {
            let sequence = self.agent_state.sequence(*session);
            if let Some(pending) = self
                .agent_observations
                .values_mut()
                .find(|pending| pending.session == *session && pending.accepts(kind, subject))
            {
                pending.record(
                    nobox_agent_wire::EventEnvelope {
                        sequence,
                        event: event.clone(),
                    },
                    now,
                );
                if kind == nobox_agent_wire::EventKind::ClientClosed
                    && subject == Some(pending.target)
                    && pending.capture_client() == Some(agent_client_id(pending.target))
                {
                    // The final capture can no longer succeed. End at the next
                    // timer boundary while retaining the correlated close and
                    // returning its structured capture error.
                    pending.minimum = Duration::ZERO;
                    pending.quiet = Duration::ZERO;
                    pending.maximum = Duration::ZERO;
                }
                rearm.push((pending.generation, pending.deadline()));
            }
        }
        for (generation, deadline) in rearm {
            if let Err(error) = self.runtime_timer.arm_agent_observation(
                generation,
                deadline.saturating_duration_since(Instant::now()),
            ) {
                self.fail_agent_observation(
                    generation,
                    AgentErrorCode::Internal,
                    &error.to_string(),
                );
            }
        }
        let subscribed_events = events
            .into_iter()
            .filter(|(session, _)| subscribers.contains(session));
        self.agent_state.publish(subscribed_events);
    }

    /// Ends action observations and discards semantic work on human control.
    fn interrupt_pending_agent_work(&mut self) {
        if let Some(pending) = self.agent_text.take() {
            let _ = self.runtime_timer.cancel_agent_text(pending.generation);
            let error = AgentError::interrupted(pending.committed.clone());
            self.finish_agent_text_error(pending, error);
        }
        let generations: Vec<u32> = self.agent_observations.keys().copied().collect();
        for generation in generations {
            let Some(pending) = self.agent_observations.remove(&generation) else {
                continue;
            };
            let _ = self.runtime_timer.cancel_agent_observation(generation);
            self.send_agent_response(
                pending.session,
                pending.request,
                pending.tool,
                AgentOutcome::Error {
                    error: AgentError::interrupted(pending.committed).with_action(pending.action),
                },
            );
        }
        let semantic_generations = self.agent_semantics.keys().copied().collect::<Vec<_>>();
        for generation in semantic_generations {
            self.cancel_agent_semantic(generation);
        }
    }

    /// Delivers queued events without ever blocking the event loop.
    ///
    /// A session whose transport is momentarily full keeps its backlog and is
    /// tried again next boundary; only a backlog that grows past its bound
    /// costs it a `resync_required`. Slow consumers degrade themselves.
    fn flush_agent_events(&mut self) {
        if self.agent_seat.is_none() || !self.agent_state.any_subscribed() {
            return;
        }
        let sessions: Vec<AgentSessionId> = self
            .agent_state
            .sessions()
            .map(|(session, _)| session)
            .collect();
        for session in sessions {
            while let Some(envelope) = self.agent_state.pop_event(session) {
                let Some(seat) = self.agent_seat.as_mut() else {
                    return;
                };
                if !seat.offer(session, AgentServerMessage::Event(envelope.clone())) {
                    self.agent_state.requeue_event(session, envelope);
                    break;
                }
            }
        }
    }

    /// Handles everything the agent I/O threads queued.
    ///
    /// Draining happens at an event-loop boundary, so an agent session always
    /// observes the manager between coherent states and never during one.
    fn drain_agent_traffic(&mut self) {
        // Everything the desktop already did is published before any request
        // is answered, so a subscription and the snapshot it continues from
        // are established at one boundary with no event between them.
        self.sync_agent_events();
        let Some(seat) = self.agent_seat.as_mut() else {
            return;
        };
        for inbound in seat.take_inbound() {
            self.handle_agent_inbound(inbound);
        }
        // Anything those requests changed is published before the loop blocks
        // again, so an agent sees the consequences of its own actions without
        // waiting for unrelated activity.
        self.sync_agent_events();
        self.flush_agent_events();
    }

    fn handle_agent_inbound(&mut self, inbound: agent::Inbound) {
        match inbound {
            agent::Inbound::Connected {
                session,
                peer,
                writer,
            } => {
                let Some(seat) = self.agent_seat.as_mut() else {
                    return;
                };
                if !seat.accept(session, peer, writer) {
                    return;
                }
                if let Some(peer) = seat.peer(session) {
                    info!(
                        session = %session,
                        uid = peer.uid,
                        pid = peer.pid,
                        executable = ?peer.executable,
                        "agent session connected"
                    );
                }
            }
            agent::Inbound::Frame { session, message } => {
                // A session that is already gone has nothing to answer with,
                // and a flood that was shed should not keep costing the
                // manager work on the way out.
                if !self
                    .agent_seat
                    .as_ref()
                    .is_some_and(|seat| seat.holds(session))
                {
                    return;
                }
                match *message {
                    AgentClientMessage::Hello(hello) => self.agent_greet(session, &hello),
                    AgentClientMessage::Request(request) => self.agent_request(session, request),
                }
            }
            agent::Inbound::Faulted { session, error } => {
                if self
                    .agent_seat
                    .as_ref()
                    .is_some_and(|seat| seat.holds(session))
                {
                    self.agent_fault(session, &error);
                }
            }
            agent::Inbound::Disconnected { session } => {
                if self
                    .agent_seat
                    .as_mut()
                    .is_some_and(|seat| seat.forget(session))
                {
                    info!(session = %session, "agent session disconnected");
                }
                self.close_agent_session(session);
            }
        }
    }

    /// Answers a handshake with the grant the manager actually issued.
    fn agent_greet(&mut self, session: AgentSessionId, hello: &nobox_agent_wire::Hello) {
        if self
            .agent_seat
            .as_ref()
            .is_some_and(|seat| seat.greeted(session))
        {
            self.agent_fault(
                session,
                &AgentError::new(
                    AgentErrorCode::HandshakeOrder,
                    "the session already greeted",
                ),
            );
            return;
        }
        if let Err(error) = hello.validate() {
            self.agent_fault(session, &error);
            return;
        }
        let Some(peer) = self.agent_seat.as_ref().and_then(|seat| seat.peer(session)) else {
            return;
        };
        let uid = peer.uid;
        let pid = peer.pid;
        let executable = peer.executable.clone();
        // Authorization is decided against the verified executable behind the
        // socket. Nothing the companion declared about itself takes part.
        let configured = self
            .config
            .agent
            .grant_for(executable.as_deref(), uid)
            .cloned();
        let grant = match &configured {
            Some(configured) if configured.scope.is_some() => {
                AgentGrant::scoped(configured.capabilities())
            }
            Some(configured) => AgentGrant::new(configured.capabilities()),
            None => AgentGrant::denied(),
        };
        if let Some(scope) = configured.as_ref().and_then(|grant| grant.scope.clone()) {
            self.agent_scopes.insert(session, scope);
        }
        let pending = PendingConsent {
            session,
            hello: hello.clone(),
            uid,
            pid,
            executable: executable.clone(),
        };
        if configured.is_none()
            && self.config.agent.policy == nobox_config::AgentPolicy::Ask
            && !hello.requested.is_empty()
        {
            // No stored grant and something was asked for: a person decides,
            // and the session waits rather than being told anything yet.
            if let Err(error) = self.begin_agent_consent(pending) {
                warn!(%error, "could not ask about an agent session");
                self.complete_agent_greeting(
                    &PendingConsent {
                        session,
                        hello: hello.clone(),
                        uid,
                        pid,
                        executable,
                    },
                    AgentGrant::denied(),
                );
            }
            return;
        }
        self.complete_agent_greeting(&pending, grant);
    }

    /// Opens the session and answers its handshake with the grant it holds.
    fn complete_agent_greeting(&mut self, pending: &PendingConsent, grant: AgentGrant) {
        let session = pending.session;
        let hello = &pending.hello;
        let granted = grant.capabilities();
        let scoped = grant.is_scoped();
        self.agent_state.open(session, grant);
        // Seed scope membership and visibility for everything already managed,
        // so a session that connects mid-run sees the same desktop as one that
        // was present from the start.
        for client in self.clients.management_order().collect::<Vec<_>>() {
            self.register_agent_client(client);
        }
        info!(
            session = %session,
            uid = pending.uid,
            pid = pending.pid,
            executable = ?pending.executable,
            harness = %hello.harness,
            purpose = %hello.purpose,
            requested = ?hello.requested,
            granted = ?granted.atoms(),
            scoped,
            "agent session greeted"
        );
        let welcome = AgentServerMessage::Welcome(nobox_agent_wire::Welcome {
            protocol: nobox_agent_wire::PROTOCOL_NAME.to_owned(),
            version: nobox_agent_wire::PROTOCOL_VERSION,
            manager: format!("nobox {}", env!("CARGO_PKG_VERSION")),
            session,
            nonce: agent::nonce(),
            granted,
            scoped,
            sequence: self.agent_state.sequence(session),
            // Only what this manager can actually perform is advertised, so a
            // harness never plans around a capability the backend lacks.
            features: self.agent_features(),
        });
        if let Some(seat) = self.agent_seat.as_mut() {
            seat.mark_greeted(session, hello.harness.clone());
            seat.send(session, welcome);
        }
        if let Err(error) = self.refresh_agent_indicator() {
            warn!(%error, "could not show the agent seat indicator");
        }
    }

    /// Returns what this backend can actually do, as opposed to what the
    /// protocol can express.
    fn agent_features(&self) -> Vec<nobox_agent_wire::Feature> {
        let mut features = vec![
            nobox_agent_wire::Feature::InputInjection,
            nobox_agent_wire::Feature::OutputCapture,
        ];
        if self.composite_version.is_some() {
            features.push(nobox_agent_wire::Feature::ObscuredCapture);
        }
        features
    }

    fn agent_request(&mut self, session: AgentSessionId, request: nobox_agent_wire::Request) {
        if !self
            .agent_seat
            .as_ref()
            .is_some_and(|seat| seat.greeted(session))
        {
            self.agent_fault(
                session,
                &AgentError::new(
                    AgentErrorCode::HandshakeOrder,
                    "greet before making requests",
                ),
            );
            return;
        }
        let tool = request.call.tool();
        if self.agent_text.is_some()
            && matches!(
                &request.call,
                nobox_agent_wire::Call::ClientPointer { .. }
                    | nobox_agent_wire::Call::ClientKey { .. }
                    | nobox_agent_wire::Call::ClientType { .. }
            )
        {
            self.send_agent_response(
                session,
                request.id,
                tool,
                AgentOutcome::Error {
                    error: AgentError::new(
                        AgentErrorCode::Internal,
                        "another text injection is still in progress",
                    ),
                },
            );
            return;
        }
        if request.call.observation().is_some()
            && self
                .agent_observations
                .values()
                .any(|pending| pending.session == session)
        {
            self.send_agent_response(
                session,
                request.id,
                tool,
                AgentOutcome::Error {
                    error: AgentError::invalid_argument(
                        "/observe",
                        nobox_agent_wire::Expected::kind(nobox_agent_wire::ExpectedKind::Object),
                        nobox_agent_wire::ReceivedKind::Object,
                        "wait for the previous observed action to finish",
                    ),
                },
            );
            return;
        }
        match self.agent_call(session, request.id, &request.call) {
            AgentCallResult::Ready(outcome) => {
                self.send_agent_response(session, request.id, tool, outcome);
            }
            AgentCallResult::DeferredObservation(pending) => {
                let generation = pending.generation;
                let timeout = pending.deadline().saturating_duration_since(Instant::now());
                self.agent_observations.insert(generation, pending);
                if let Err(error) = self
                    .runtime_timer
                    .arm_agent_observation(generation, timeout)
                {
                    let pending = self
                        .agent_observations
                        .remove(&generation)
                        .expect("the pending observation was just inserted");
                    let mut protocol_error =
                        AgentError::new(AgentErrorCode::Internal, error.to_string());
                    protocol_error.committed = pending.committed;
                    protocol_error.action = Some(pending.action);
                    self.send_agent_response(
                        pending.session,
                        pending.request,
                        pending.tool,
                        AgentOutcome::Error {
                            error: protocol_error,
                        },
                    );
                }
            }
            AgentCallResult::DeferredText(pending) => {
                let generation = pending.generation;
                self.agent_text = Some(pending);
                if let Err(error) = self
                    .runtime_timer
                    .arm_agent_text(generation, Duration::ZERO)
                {
                    let pending = self
                        .agent_text
                        .take()
                        .expect("the pending text request was just inserted");
                    self.send_agent_response(
                        pending.session,
                        pending.request,
                        pending.tool,
                        AgentOutcome::Error {
                            error: AgentError::new(AgentErrorCode::Internal, error.to_string()),
                        },
                    );
                }
            }
            AgentCallResult::DeferredSemantic {
                pending,
                helper_request,
            } => {
                let generation = pending.generation;
                self.agent_semantics.insert(generation, *pending);
                if let Err(error) = self
                    .runtime_timer
                    .arm_agent_semantic(generation, SEMANTIC_REPLY_DELAY)
                {
                    self.agent_semantics.remove(&generation);
                    self.send_agent_response(
                        session,
                        request.id,
                        tool,
                        AgentOutcome::Error {
                            error: AgentError::new(AgentErrorCode::Internal, error.to_string()),
                        },
                    );
                    return;
                }
                let started = helper_request.is_some_and(|helper_request| {
                    self.semantic_runner
                        .as_ref()
                        .is_some_and(|runner| runner.start(generation, helper_request))
                });
                if !started && let Some(pending) = self.agent_semantics.get_mut(&generation) {
                    pending.result = Some(semantic::Result::Unavailable);
                }
            }
        }
    }

    /// Logs and sends one completed request outcome.
    fn send_agent_response(
        &mut self,
        session: AgentSessionId,
        request: AgentRequestId,
        tool: &'static str,
        outcome: AgentOutcome,
    ) {
        let harness = self
            .agent_seat
            .as_ref()
            .map(|seat| seat.harness(session).to_owned())
            .unwrap_or_default();
        // Every agent action is attributable: session, declared harness, and
        // the verified process behind the socket.
        let (uid, pid) = self
            .agent_seat
            .as_ref()
            .and_then(|seat| seat.peer(session))
            .map_or((0, 0), |peer| (peer.uid, peer.pid));
        match outcome.code() {
            None => info!(session = %session, %harness, uid, pid, tool, "agent request served"),
            Some(code) => info!(
                session = %session,
                %harness,
                uid,
                pid,
                tool,
                refusal = code.as_str(),
                "agent request refused"
            ),
        }
        let response = AgentServerMessage::Response(nobox_agent_wire::Response {
            id: request,
            sequence: self.agent_state.sequence(session),
            outcome,
        });
        if let Some(seat) = self.agent_seat.as_mut() {
            seat.send(session, response);
        }
    }

    /// Evaluates one tool call against the session's grant and the live desktop.
    fn agent_call(
        &mut self,
        session: AgentSessionId,
        request: AgentRequestId,
        call: &nobox_agent_wire::Call,
    ) -> AgentCallResult {
        if let Err(error) = call.validate() {
            return AgentOutcome::Error { error }.into();
        }
        if let Err(error) = self.agent_state.authorize(session, call) {
            return AgentOutcome::Error { error }.into();
        }
        match call {
            nobox_agent_wire::Call::DesktopSnapshot {} => AgentOutcome::Ok {
                reply: AgentReply::Snapshot {
                    snapshot: self.agent_state.snapshot(
                        session,
                        &self.clients,
                        &self.outputs,
                        self,
                    ),
                },
            }
            .into(),
            nobox_agent_wire::Call::SubscribeAndSnapshot { kinds } => {
                self.agent_state.subscribe(session, kinds);
                AgentOutcome::Ok {
                    reply: AgentReply::Subscribed {
                        kinds: if kinds.is_empty() {
                            nobox_agent_wire::EventKind::ALL.to_vec()
                        } else {
                            kinds.clone()
                        },
                        snapshot: self.agent_state.snapshot(
                            session,
                            &self.clients,
                            &self.outputs,
                            self,
                        ),
                    },
                }
                .into()
            }
            nobox_agent_wire::Call::ClientGet { client } => {
                match self.agent_state.descriptor(
                    session,
                    client_id_from_agent(*client),
                    &self.clients,
                    &self.outputs,
                    self,
                ) {
                    // Hidden, out of scope, and never-existed are one answer.
                    None => AgentOutcome::Error {
                        error: AgentError::no_such_client(),
                    },
                    Some(descriptor) => AgentOutcome::Ok {
                        reply: AgentReply::Client { client: descriptor },
                    },
                }
                .into()
            }
            nobox_agent_wire::Call::ClientSemanticRoot { client }
            | nobox_agent_wire::Call::ClientSemanticTree { client, .. }
            | nobox_agent_wire::Call::ClientSemanticFind { client, .. } => {
                let native = client_id_from_agent(*client);
                let Some(descriptor) = self.agent_state.descriptor(
                    session,
                    native,
                    &self.clients,
                    &self.outputs,
                    self,
                ) else {
                    return AgentOutcome::Error {
                        error: AgentError::no_such_client(),
                    }
                    .into();
                };
                if descriptor.redacted {
                    return AgentOutcome::Error {
                        error: AgentError::semantic_unavailable(),
                    }
                    .into();
                }
                let (projection, search, helper_projection, helper_search) = match call {
                    nobox_agent_wire::Call::ClientSemanticTree {
                        root,
                        continuation,
                        max_nodes,
                        max_depth,
                        ..
                    } => {
                        let Some(tree) = self.agent_semantic_trees.get(&(session, native)) else {
                            return AgentOutcome::Error {
                                error: AgentError::semantic_unavailable(),
                            }
                            .into();
                        };
                        let (root, offset, max_depth, source_continuation) =
                            if let Some(continuation) = continuation {
                                let Some(AgentSemanticCursor::Tree {
                                    root,
                                    offset,
                                    max_depth,
                                }) = tree.continuations.get(continuation).cloned()
                                else {
                                    return AgentOutcome::Error {
                                        error: AgentError::stale_tree(tree.generation),
                                    }
                                    .into();
                                };
                                (root, offset, max_depth, Some(*continuation))
                            } else if let Some(root) = root {
                                if root.tree != tree.generation {
                                    return AgentOutcome::Error {
                                        error: AgentError::stale_tree(tree.generation),
                                    }
                                    .into();
                                }
                                let Some(internal) = tree.internal_by_public.get(&root.node) else {
                                    return AgentOutcome::Error {
                                        error: AgentError::stale_tree(tree.generation),
                                    }
                                    .into();
                                };
                                (*internal, 0, *max_depth, None)
                            } else {
                                (tree.root, 0, *max_depth, None)
                            };
                        let pending = PendingSemanticProjection {
                            tree_generation: tree.generation,
                            root,
                            offset,
                            max_nodes: *max_nodes,
                            max_depth,
                            source_continuation,
                        };
                        (
                            Some(pending),
                            None,
                            Some(semantic::Projection::new(
                                root, offset, *max_nodes, max_depth,
                            )),
                            None,
                        )
                    }
                    nobox_agent_wire::Call::ClientSemanticFind {
                        query,
                        continuation,
                        max_results,
                        ..
                    } => {
                        let Some(tree) = self.agent_semantic_trees.get(&(session, native)) else {
                            return AgentOutcome::Error {
                                error: AgentError::semantic_unavailable(),
                            }
                            .into();
                        };
                        let (offset, query, source_continuation) =
                            if let Some(continuation) = continuation {
                                let Some(AgentSemanticCursor::Search { offset, query }) =
                                    tree.continuations.get(continuation).cloned()
                                else {
                                    return AgentOutcome::Error {
                                        error: AgentError::stale_tree(tree.generation),
                                    }
                                    .into();
                                };
                                (offset, query, Some(*continuation))
                            } else {
                                (0, query.clone(), None)
                            };
                        let pending = PendingSemanticSearch {
                            tree_generation: tree.generation,
                            offset,
                            max_results: *max_results,
                            query: query.clone(),
                            source_continuation,
                        };
                        (
                            None,
                            Some(pending),
                            None,
                            Some(semantic::Search::new(offset, *max_results, query)),
                        )
                    }
                    nobox_agent_wire::Call::ClientSemanticRoot { .. } => (None, None, None, None),
                    _ => unreachable!("only semantic calls enter this branch"),
                };
                let pid = xres_local_client_pid(&self.connection, window_id(native));
                let helper_request = pid.and_then(|pid| {
                    let clients = self.clients.management_order().collect::<Vec<_>>();
                    if clients.len() > MAX_SEMANTIC_CLIENT_SCAN {
                        return None;
                    }
                    let mut complete = true;
                    let mut owned = 0_usize;
                    for candidate in clients {
                        match xres_local_client_pid(&self.connection, window_id(candidate)) {
                            Some(candidate_pid) if candidate_pid == pid => owned += 1,
                            Some(_) => {}
                            None => complete = false,
                        }
                    }
                    let content = semantic_rect(descriptor.content)?;
                    let frame = semantic_rect(descriptor.frame)?;
                    let mut rects = vec![content];
                    if frame != content {
                        rects.push(frame);
                    }
                    let request = semantic::Request::new(pid, rects, complete && owned == 1)?;
                    Some(match (helper_projection, helper_search) {
                        (Some(projection), None) => request.with_projection(projection),
                        (None, Some(search)) => request.with_search(search),
                        (None, None) => request,
                        (Some(_), Some(_)) => return None,
                    })
                });
                self.agent_semantic_generation = self.agent_semantic_generation.wrapping_add(1);
                AgentCallResult::DeferredSemantic {
                    pending: Box::new(PendingAgentSemantic {
                        generation: self.agent_semantic_generation,
                        session,
                        request,
                        tool: call.tool(),
                        call: call.clone(),
                        target: native,
                        client_generation: descriptor.generation,
                        pid: pid.unwrap_or_default(),
                        cancelled: false,
                        result: helper_request
                            .is_none()
                            .then_some(semantic::Result::Unavailable),
                        projection,
                        search,
                    }),
                    helper_request,
                }
            }
            nobox_agent_wire::Call::Launch {
                desktop_entry,
                uris,
            } => self.agent_launch(session, desktop_entry, uris).into(),
            nobox_agent_wire::Call::ClientCapture {
                client,
                area,
                rect,
                grid,
                expects,
            } => self
                .agent_capture_client(session, *client, *area, *rect, *grid, expects)
                .into(),
            nobox_agent_wire::Call::OutputCapture { output, rect } => {
                self.agent_capture_output(session, *output, *rect).into()
            }
            nobox_agent_wire::Call::ClientPointer {
                client,
                x,
                y,
                action,
                button,
                ensure_visible,
                expects,
                observe,
            } => {
                let (x, y, action, button) = (*x, *y, *action, *button);
                self.agent_input_action(
                    AgentInputRequest {
                        session,
                        request,
                        tool: call.tool(),
                        client: *client,
                        expects,
                        ensure_visible: *ensure_visible,
                        observe: *observe,
                    },
                    false,
                    |_| Ok(()),
                    |manager, id, ()| manager.agent_inject_pointer(id, x, y, action, button),
                )
            }
            nobox_agent_wire::Call::ClientKey {
                client,
                key,
                action,
                modifiers,
                ensure_visible,
                expects,
                observe,
            } => {
                let key = key.clone();
                let action = *action;
                let modifiers = modifiers.clone();
                self.agent_input_action(
                    AgentInputRequest {
                        session,
                        request,
                        tool: call.tool(),
                        client: *client,
                        expects,
                        ensure_visible: *ensure_visible,
                        observe: *observe,
                    },
                    true,
                    |_| Ok(()),
                    |manager, _, ()| manager.agent_inject_key(&key, action, &modifiers),
                )
            }
            nobox_agent_wire::Call::ClientType {
                client,
                text,
                ensure_visible,
                expects,
                observe,
            } => self.agent_type_action(
                AgentInputRequest {
                    session,
                    request,
                    tool: call.tool(),
                    client: *client,
                    expects,
                    ensure_visible: *ensure_visible,
                    observe: *observe,
                },
                text,
            ),
            nobox_agent_wire::Call::ClientActivate { client, expects } => {
                self.agent_client_action(session, *client, expects, |manager, id, timestamp| {
                    let before = manager.clients.current_workspace();
                    // Activation routes through the manager's own focus
                    // contract, as a pager's would.
                    manager.activate_client(id, timestamp, false)?;
                    let mut committed = Vec::new();
                    if manager.clients.current_workspace() != before {
                        committed.push(AgentStep::WorkspaceSwitch);
                    }
                    committed.push(AgentStep::Activate);
                    Ok(committed)
                })
                .into()
            }
            nobox_agent_wire::Call::ClientClose { client, expects } => {
                let closable = self
                    .clients
                    .get(client_id_from_agent(*client))
                    .is_some_and(|managed| managed.policy.capabilities.closable);
                if !closable {
                    return AgentOutcome::Error {
                        error: AgentError::new(
                            AgentErrorCode::Unsupported,
                            "this client cannot be closed through its own protocol",
                        ),
                    }
                    .into();
                }
                self.agent_client_action(session, *client, expects, |manager, id, timestamp| {
                    // Negotiated close only: the protocol never exposes a kill.
                    manager.close_client(id, timestamp)?;
                    Ok(vec![AgentStep::Close])
                })
                .into()
            }
            nobox_agent_wire::Call::ClientMoveResize {
                client,
                geometry,
                expects,
            } => {
                let geometry = *geometry;
                self.agent_client_action(session, *client, expects, |manager, id, _| {
                    let gravity = manager
                        .clients
                        .get(id)
                        .map_or(Gravity::NorthWest, |managed| managed.gravity);
                    manager.configure_managed_geometry(
                        id,
                        GeometryRequest {
                            x: geometry.x,
                            y: geometry.y,
                            width: geometry.width,
                            height: geometry.height,
                            gravity,
                        },
                    )?;
                    Ok(vec![AgentStep::Geometry])
                })
                .into()
            }
            nobox_agent_wire::Call::ClientSetState {
                client,
                change,
                expects,
            } => {
                let change = *change;
                self.agent_client_action(session, *client, expects, |manager, id, timestamp| {
                    manager.agent_apply_state(id, &change, timestamp)
                })
                .into()
            }
            nobox_agent_wire::Call::ClientSendToWorkspace {
                client,
                workspace,
                follow,
                expects,
            } => {
                if workspace.raw() >= self.clients.workspace_count() {
                    return AgentOutcome::Error {
                        error: AgentError::new(AgentErrorCode::NoSuchTarget, "no such workspace"),
                    }
                    .into();
                }
                let destination = WorkspaceId::new(workspace.raw());
                let follow = *follow;
                self.agent_client_action(session, *client, expects, |manager, id, timestamp| {
                    manager.move_to_workspace(
                        id,
                        WorkspaceAssignment::Workspace(destination),
                        timestamp,
                        follow,
                    )?;
                    let mut committed = vec![AgentStep::Assign];
                    if follow {
                        committed.push(AgentStep::WorkspaceSwitch);
                    }
                    Ok(committed)
                })
                .into()
            }
            nobox_agent_wire::Call::WorkspaceSwitch { workspace } => {
                if workspace.raw() >= self.clients.workspace_count() {
                    return AgentOutcome::Error {
                        error: AgentError::new(AgentErrorCode::NoSuchTarget, "no such workspace"),
                    }
                    .into();
                }
                let timestamp = self.last_timestamp;
                match self.switch_workspace(WorkspaceId::new(workspace.raw()), timestamp) {
                    Ok(()) => AgentOutcome::Ok {
                        reply: AgentReply::Committed {
                            committed: vec![AgentStep::WorkspaceSwitch],
                            sequence: self.agent_state.sequence(session),
                        },
                    },
                    Err(error) => AgentOutcome::Error {
                        error: AgentError::new(AgentErrorCode::Internal, error.to_string()),
                    },
                }
                .into()
            }
        }
    }

    /// Runs one client-addressed action behind the checks every mutating call
    /// shares: the session must perceive the client, and the state it says it
    /// observed must still hold.
    fn agent_client_action(
        &mut self,
        session: AgentSessionId,
        client: nobox_agent_wire::ClientId,
        expects: &nobox_agent_wire::Expects,
        action: impl FnOnce(&mut Self, ClientId, u32) -> Result<Vec<AgentStep>, X11Error>,
    ) -> AgentOutcome {
        let target = client_id_from_agent(client);
        if !self.agent_state.perceives(session, target) {
            return AgentOutcome::Error {
                error: AgentError::no_such_client(),
            };
        }
        if let Err(error) = self
            .agent_state
            .check_expects(target, expects, &self.clients)
        {
            return AgentOutcome::Error { error };
        }
        let timestamp = self.last_timestamp;
        match action(self, target, timestamp) {
            Ok(committed) => AgentOutcome::Ok {
                reply: AgentReply::Committed {
                    committed,
                    sequence: self.agent_state.sequence(session),
                },
            },
            Err(error) => {
                warn!(session = %session, %error, "an agent action failed inside the manager");
                AgentOutcome::Error {
                    error: AgentError::new(AgentErrorCode::Internal, error.to_string()),
                }
            }
        }
    }

    /// Applies a requested state change through the manager's ordinary paths.
    ///
    /// The agent surface adds an intent source, not new state machinery: every
    /// change here is the same one a key binding or a pager would make.
    fn agent_apply_state(
        &mut self,
        client: ClientId,
        change: &nobox_agent_wire::StateChange,
        timestamp: u32,
    ) -> Result<Vec<AgentStep>, X11Error> {
        let window = window_id(client);
        let Some(current) = self.clients.get(client).copied() else {
            return Ok(Vec::new());
        };
        if let Some(minimized) = change.minimized {
            if minimized {
                self.iconify(window)?;
            } else {
                self.restore(window)?;
            }
        }
        if change.maximized_horizontal.is_some() || change.maximized_vertical.is_some() {
            let (horizontal, vertical) = current.maximize.map_or((false, false), |maximize| {
                (maximize.horizontal, maximize.vertical)
            });
            self.set_maximized(
                window,
                change.maximized_horizontal.unwrap_or(horizontal),
                change.maximized_vertical.unwrap_or(vertical),
            )?;
        }
        if let Some(fullscreen) = change.fullscreen {
            self.set_fullscreen(window, fullscreen)?;
        }
        if let Some(shaded) = change.shaded {
            self.set_shaded(window, shaded)?;
        }
        if let Some(sticky) = change.sticky {
            let assignment = if sticky {
                WorkspaceAssignment::All
            } else {
                WorkspaceAssignment::Workspace(self.clients.current_workspace())
            };
            self.move_to_workspace(client, assignment, timestamp, false)?;
        }
        if let Some(above) = change.above {
            let layer = if above {
                ClientLayer::Above
            } else if matches!(current.layer, ClientLayer::Above) {
                ClientLayer::Normal
            } else {
                current.layer
            };
            self.set_client_layer(window, layer)?;
        }
        if let Some(below) = change.below {
            let current = self.clients.get(client).map_or(current.layer, |c| c.layer);
            let layer = if below {
                ClientLayer::Below
            } else if matches!(current, ClientLayer::Below) {
                ClientLayer::Normal
            } else {
                current
            };
            self.set_client_layer(window, layer)?;
        }
        Ok(vec![AgentStep::State])
    }

    /// Runs one input call: perceive, check freshness, yield to the human,
    /// optionally make the target visible, and only then inject.
    ///
    /// Every step that commits is recorded before the next one is attempted,
    /// so a sequence preempted half-way reports exactly where it stopped. No
    /// request reports full success after human preemption.
    fn prepare_agent_input<P>(
        &mut self,
        request: AgentInputRequest<'_>,
        prepare: impl FnOnce(&Self) -> Result<P, X11Error>,
    ) -> Result<(ClientId, P, Vec<AgentStep>), AgentError> {
        let target = client_id_from_agent(request.client);
        if !self.agent_state.perceives(request.session, target) {
            return Err(AgentError::no_such_client());
        }
        if matches!(
            self.agent_state.visibility(target),
            AgentClientVisibility::Redacted
        ) {
            return Err(AgentError::denied(
                "this client is redacted; input is refused",
            ));
        }
        self.agent_state
            .check_expects(target, request.expects, &self.clients)?;
        // Deterministic input validation happens before ensure_visible can
        // switch workspaces, activate, or raise the target. In particular,
        // client_type resolves the entire string here so an unsupported suffix
        // can never leave an injected prefix behind.
        let prepared = prepare(self).map_err(|error| match error {
            X11Error::AgentInput(message) => {
                AgentError::new(AgentErrorCode::InvalidArgument, message)
            }
            error => AgentError::new(AgentErrorCode::Internal, error.to_string()),
        })?;
        let mut committed = Vec::new();
        if self.agent_input_suppressed() {
            return Err(AgentError::interrupted(committed));
        }
        if request.ensure_visible {
            let timestamp = self.last_timestamp;
            let before = self.clients.current_workspace();
            self.activate_client(target, timestamp, false)
                .map_err(|error| AgentError::new(AgentErrorCode::Internal, error.to_string()))?;
            if self.clients.current_workspace() != before {
                committed.push(AgentStep::WorkspaceSwitch);
            }
            committed.push(AgentStep::Activate);
            committed.push(AgentStep::Raise);
            // The human may have acted while that was happening. Steps already
            // committed stay committed; nothing further is attempted.
            if self.agent_input_suppressed() {
                return Err(AgentError::interrupted(committed));
            }
        }
        // Geometry is read again here, not where the call was parsed: the
        // window may have moved since, and an agent's coordinates are relative
        // to the window rather than to the screen.
        if self.clients.get(target).is_none() {
            return Err(AgentError::no_such_client());
        }
        Ok((target, prepared, committed))
    }

    /// Starts a validated text request as one paced event-loop operation.
    fn agent_type_action(&mut self, request: AgentInputRequest<'_>, text: &str) -> AgentCallResult {
        if self.agent_text.is_some() {
            return AgentOutcome::Error {
                error: AgentError::new(
                    AgentErrorCode::Internal,
                    "another text injection is still in progress",
                ),
            }
            .into();
        }
        let requested_target = client_id_from_agent(request.client);
        let (target, plan, committed) = match self.prepare_agent_input(request, |manager| {
            manager.agent_text_plan(requested_target, text)
        }) {
            Ok(prepared) => prepared,
            Err(error) => return AgentOutcome::Error { error }.into(),
        };
        if self.clients.focused() != Some(target) {
            return AgentOutcome::Error {
                error: AgentError::stale_state(self.agent_state.generation(target)),
            }
            .into();
        }
        let Some(action) = self.agent_state.issue_action(request.session) else {
            return AgentOutcome::Error {
                error: AgentError::new(
                    AgentErrorCode::Internal,
                    "the agent session ended before text injection",
                ),
            }
            .into();
        };
        self.agent_text_generation = self.agent_text_generation.wrapping_add(1);
        AgentCallResult::DeferredText(PendingAgentText {
            generation: self.agent_text_generation,
            session: request.session,
            request: request.request,
            tool: request.tool,
            call: nobox_agent_wire::Call::ClientType {
                client: request.client,
                text: text.to_owned(),
                ensure_visible: request.ensure_visible,
                expects: *request.expects,
                observe: request.observe,
            },
            target,
            plan: match plan {
                AgentTextPlan::Strokes(strokes) => PendingAgentTextPlan::Strokes(strokes.into()),
                AgentTextPlan::Transfer {
                    text,
                    client_base,
                    paste,
                } => PendingAgentTextPlan::TransferPending {
                    text,
                    client_base,
                    paste,
                },
            },
            committed,
            action,
            observe: request.observe,
        })
    }

    fn agent_input_action<P>(
        &mut self,
        request: AgentInputRequest<'_>,
        requires_focus: bool,
        prepare: impl FnOnce(&Self) -> Result<P, X11Error>,
        inject: impl FnOnce(&mut Self, ClientId, P) -> Result<(), X11Error>,
    ) -> AgentCallResult {
        let (target, prepared, mut committed) = match self.prepare_agent_input(request, prepare) {
            Ok(prepared) => prepared,
            Err(error) => return AgentOutcome::Error { error }.into(),
        };
        if requires_focus && self.clients.focused() != Some(target) {
            return AgentOutcome::Error {
                error: AgentError::stale_state(self.agent_state.generation(target)),
            }
            .into();
        }
        match inject(self, target, prepared) {
            Ok(()) => {
                committed.push(AgentStep::Inject);
                self.mark_agent_input_target(target);
                let Some(action) = self.agent_state.issue_action(request.session) else {
                    let mut error = AgentError::new(
                        AgentErrorCode::Internal,
                        "the agent session ended during input injection",
                    );
                    error.committed = committed;
                    return AgentOutcome::Error { error }.into();
                };
                // Everything before the injection is manager-owned state this
                // code watched change. The injection itself is not: the events
                // left here addressed to this client, and what the client did
                // with them is outside anything this process can see.
                let started = Instant::now();
                let started_sequence = self.agent_state.sequence(request.session);
                let Some(observe) = request.observe else {
                    return AgentOutcome::Ok {
                        reply: AgentReply::Injected {
                            action,
                            committed,
                            delivery: nobox_agent_wire::Delivery::Unverified,
                            sequence: started_sequence,
                            observation: None,
                        },
                    }
                    .into();
                };
                self.agent_observation_generation =
                    self.agent_observation_generation.wrapping_add(1);
                AgentCallResult::DeferredObservation(PendingAgentObservation {
                    generation: self.agent_observation_generation,
                    session: request.session,
                    request: request.request,
                    tool: request.tool,
                    action,
                    target,
                    capture: observe.capture,
                    committed,
                    started,
                    started_sequence,
                    minimum: Duration::from_millis(u64::from(observe.minimum_ms)),
                    quiet: Duration::from_millis(u64::from(observe.quiet_ms)),
                    maximum: Duration::from_millis(u64::from(observe.maximum_ms)),
                    last_event: started,
                    events: Vec::new(),
                    dropped_events: 0,
                })
            }
            Err(X11Error::AgentTargetChanged) => AgentOutcome::Error {
                error: AgentError::stale_state(self.agent_state.generation(target)),
            }
            .into(),
            Err(X11Error::AgentInput(message)) => AgentOutcome::Error {
                error: AgentError::new(AgentErrorCode::InvalidArgument, message),
            }
            .into(),
            Err(error) => AgentOutcome::Error {
                error: AgentError::new(AgentErrorCode::Internal, error.to_string()),
            }
            .into(),
        }
    }

    /// Advances a validated text operation by one event-loop boundary.
    ///
    /// Layout-representable text emits one complete character. Other valid
    /// UTF-8 starts one target-scoped selection offer and balanced paste chord.
    fn advance_agent_text(&mut self, generation: u32) {
        let Some(mut pending) = self.agent_text.take() else {
            return;
        };
        if pending.generation != generation {
            self.agent_text = Some(pending);
            return;
        }
        if let Err(error) = self.agent_state.authorize(pending.session, &pending.call) {
            self.finish_agent_text_error(pending, error);
            return;
        }
        if !self.agent_state.perceives(pending.session, pending.target)
            || matches!(
                self.agent_state.visibility(pending.target),
                AgentClientVisibility::Redacted
            )
            || self.clients.get(pending.target).is_none()
        {
            self.finish_agent_text_error(pending, AgentError::no_such_client());
            return;
        }
        if self.clients.focused() != Some(pending.target) {
            let error = AgentError::stale_state(self.agent_state.generation(pending.target));
            self.finish_agent_text_error(pending, error);
            return;
        }
        if self.agent_input_suppressed() {
            let error = AgentError::interrupted(pending.committed.clone());
            self.finish_agent_text_error(pending, error);
            return;
        }
        if let PendingAgentTextPlan::TransferOffered {
            deadline,
            last_delivery,
            ..
        } = pending.plan
        {
            let now = Instant::now();
            let finish_at = agent_text_transfer_finish_at(deadline, last_delivery);
            if now < finish_at {
                self.agent_text = Some(pending);
                if let Err(error) = self
                    .runtime_timer
                    .arm_agent_text(generation, finish_at.saturating_duration_since(now))
                {
                    let pending = self
                        .agent_text
                        .take()
                        .expect("the exact-text request was just restored");
                    let _ = self.release_agent_text_selection();
                    self.finish_agent_text_error(
                        pending,
                        AgentError::new(AgentErrorCode::Internal, error.to_string()),
                    );
                }
                return;
            }
            if let Err(error) = self.release_agent_text_selection() {
                self.finish_agent_text_error(
                    pending,
                    AgentError::new(AgentErrorCode::Internal, error.to_string()),
                );
            } else {
                self.finish_agent_text_success(pending);
            }
            return;
        }

        let transfer = match &pending.plan {
            PendingAgentTextPlan::TransferPending {
                client_base, paste, ..
            } => Some((*client_base, *paste)),
            PendingAgentTextPlan::Strokes(_) | PendingAgentTextPlan::TransferOffered { .. } => None,
        };
        if let Some((client_base, paste)) = transfer {
            if let Err(error) = self.begin_agent_text_transfer(paste) {
                let _ = self.release_agent_text_selection();
                self.finish_agent_text_error(
                    pending,
                    AgentError::new(AgentErrorCode::Internal, error.to_string()),
                );
                return;
            }
            let text = match std::mem::replace(
                &mut pending.plan,
                PendingAgentTextPlan::Strokes(VecDeque::new()),
            ) {
                PendingAgentTextPlan::TransferPending { text, .. } => text,
                PendingAgentTextPlan::Strokes(_) | PendingAgentTextPlan::TransferOffered { .. } => {
                    unreachable!("the pending transfer plan was just inspected")
                }
            };
            pending.plan = PendingAgentTextPlan::TransferOffered {
                text,
                client_base,
                deadline: Instant::now() + AGENT_TEXT_TRANSFER_TIMEOUT,
                last_delivery: None,
            };
            pending.committed.push(AgentStep::Inject);
            self.mark_agent_input_target(pending.target);
            self.agent_text = Some(pending);
            if let Err(error) = self
                .runtime_timer
                .arm_agent_text(generation, AGENT_TEXT_TRANSFER_TIMEOUT)
            {
                let pending = self
                    .agent_text
                    .take()
                    .expect("the exact-text request was just restored");
                let _ = self.release_agent_text_selection();
                self.finish_agent_text_error(
                    pending,
                    AgentError::new(AgentErrorCode::Internal, error.to_string()),
                );
            }
            return;
        }

        let PendingAgentTextPlan::Strokes(strokes) = &mut pending.plan else {
            unreachable!("transfer plans returned above")
        };
        let Some(stroke) = strokes.pop_front() else {
            self.finish_agent_text_error(
                pending,
                AgentError::new(
                    AgentErrorCode::Internal,
                    "the paced text plan was unexpectedly empty",
                ),
            );
            return;
        };
        if let Err(error) = self.agent_inject_text_stroke(stroke) {
            self.finish_agent_text_error(
                pending,
                AgentError::new(AgentErrorCode::Internal, error.to_string()),
            );
            return;
        }
        if !pending.committed.contains(&AgentStep::Inject) {
            pending.committed.push(AgentStep::Inject);
            self.mark_agent_input_target(pending.target);
        }
        if !strokes.is_empty() {
            self.agent_text = Some(pending);
            if let Err(error) = self
                .runtime_timer
                .arm_agent_text(generation, AGENT_TEXT_STROKE_DELAY)
            {
                let pending = self
                    .agent_text
                    .take()
                    .expect("the paced text request was just restored");
                self.finish_agent_text_error(
                    pending,
                    AgentError::new(AgentErrorCode::Internal, error.to_string()),
                );
            }
            return;
        }

        self.finish_agent_text_success(pending);
    }

    fn finish_agent_text_success(&mut self, pending: PendingAgentText) {
        self.mark_agent_input_target(pending.target);
        let started = Instant::now();
        let started_sequence = self.agent_state.sequence(pending.session);
        let Some(observe) = pending.observe else {
            self.send_agent_response(
                pending.session,
                pending.request,
                pending.tool,
                AgentOutcome::Ok {
                    reply: AgentReply::Injected {
                        action: pending.action,
                        committed: pending.committed,
                        delivery: nobox_agent_wire::Delivery::Unverified,
                        sequence: started_sequence,
                        observation: None,
                    },
                },
            );
            return;
        };
        self.agent_observation_generation = self.agent_observation_generation.wrapping_add(1);
        let observation = PendingAgentObservation {
            generation: self.agent_observation_generation,
            session: pending.session,
            request: pending.request,
            tool: pending.tool,
            action: pending.action,
            target: pending.target,
            capture: observe.capture,
            committed: pending.committed,
            started,
            started_sequence,
            minimum: Duration::from_millis(u64::from(observe.minimum_ms)),
            quiet: Duration::from_millis(u64::from(observe.quiet_ms)),
            maximum: Duration::from_millis(u64::from(observe.maximum_ms)),
            last_event: started,
            events: Vec::new(),
            dropped_events: 0,
        };
        let observation_generation = observation.generation;
        let timeout = observation
            .deadline()
            .saturating_duration_since(Instant::now());
        self.agent_observations
            .insert(observation_generation, observation);
        if let Err(error) = self
            .runtime_timer
            .arm_agent_observation(observation_generation, timeout)
        {
            self.fail_agent_observation(
                observation_generation,
                AgentErrorCode::Internal,
                &error.to_string(),
            );
        }
    }

    fn finish_agent_text_error(&mut self, pending: PendingAgentText, mut error: AgentError) {
        if matches!(pending.plan, PendingAgentTextPlan::TransferOffered { .. }) {
            let _ = self.release_agent_text_selection();
        }
        error.committed = pending.committed;
        if error.committed.contains(&AgentStep::Inject) {
            error.action = Some(pending.action);
        }
        self.send_agent_response(
            pending.session,
            pending.request,
            pending.tool,
            AgentOutcome::Error { error },
        );
    }

    fn fail_session_text(&mut self, session: AgentSessionId, code: AgentErrorCode, message: &str) {
        if self
            .agent_text
            .as_ref()
            .is_none_or(|pending| pending.session != session)
        {
            return;
        }
        let pending = self
            .agent_text
            .take()
            .expect("the pending text request belonged to this session");
        let _ = self.runtime_timer.cancel_agent_text(pending.generation);
        self.finish_agent_text_error(pending, AgentError::new(code, message));
    }

    /// Completes a bounded action observation without blocking the event loop.
    fn finish_agent_observation(&mut self, generation: u32) {
        self.sync_agent_events();
        let Some(deadline) = self
            .agent_observations
            .get(&generation)
            .map(PendingAgentObservation::deadline)
        else {
            return;
        };
        let now = Instant::now();
        if now < deadline {
            if let Err(error) = self
                .runtime_timer
                .arm_agent_observation(generation, deadline.duration_since(now))
            {
                self.fail_agent_observation(
                    generation,
                    AgentErrorCode::Internal,
                    &error.to_string(),
                );
            }
            return;
        }
        let Some(pending) = self.agent_observations.remove(&generation) else {
            return;
        };
        let _ = self.runtime_timer.cancel_agent_observation(generation);
        let elapsed = u32::try_from(pending.started.elapsed().as_millis()).unwrap_or(u32::MAX);
        let samples = pending.capture.map_or_else(Vec::new, |capture| {
            let client = pending
                .capture_client()
                .expect("a present observation capture has a client");
            let capture_call = nobox_agent_wire::Call::ClientCapture {
                client,
                area: capture.area,
                rect: capture.rect,
                grid: capture.grid,
                expects: nobox_agent_wire::Expects::default(),
            };
            let sample = match self.agent_state.authorize(pending.session, &capture_call) {
                Err(error) => nobox_agent_wire::ObservationSample::Error {
                    after_ms: elapsed,
                    error,
                },
                Ok(()) => match self.agent_capture_client(
                    pending.session,
                    client,
                    capture.area,
                    capture.rect,
                    capture.grid,
                    &nobox_agent_wire::Expects::default(),
                ) {
                    AgentOutcome::Ok {
                        reply: AgentReply::Capture { image },
                    } => nobox_agent_wire::ObservationSample::Ok {
                        after_ms: elapsed,
                        image,
                    },
                    AgentOutcome::Error { error } => nobox_agent_wire::ObservationSample::Error {
                        after_ms: elapsed,
                        error,
                    },
                    AgentOutcome::Ok { .. } => nobox_agent_wire::ObservationSample::Error {
                        after_ms: elapsed,
                        error: AgentError::new(
                            AgentErrorCode::Internal,
                            "capture returned an unexpected reply",
                        ),
                    },
                },
            };
            vec![sample]
        });
        let finished_sequence = self.agent_state.sequence(pending.session);
        self.send_agent_response(
            pending.session,
            pending.request,
            pending.tool,
            AgentOutcome::Ok {
                reply: AgentReply::Injected {
                    action: pending.action,
                    committed: pending.committed,
                    delivery: nobox_agent_wire::Delivery::Unverified,
                    sequence: finished_sequence,
                    observation: Some(nobox_agent_wire::ActionObservation {
                        started_sequence: pending.started_sequence,
                        finished_sequence,
                        elapsed_ms: elapsed,
                        events: pending.events,
                        dropped_events: pending.dropped_events,
                        samples,
                    }),
                },
            },
        );
    }

    fn fail_agent_observation(&mut self, generation: u32, code: AgentErrorCode, message: &str) {
        let Some(pending) = self.agent_observations.remove(&generation) else {
            return;
        };
        let _ = self.runtime_timer.cancel_agent_observation(generation);
        let mut error = AgentError::new(code, message);
        error.committed = pending.committed;
        error.action = Some(pending.action);
        self.send_agent_response(
            pending.session,
            pending.request,
            pending.tool,
            AgentOutcome::Error { error },
        );
    }

    fn fail_session_observations(
        &mut self,
        session: AgentSessionId,
        code: AgentErrorCode,
        message: &str,
    ) {
        let generations: Vec<u32> = self
            .agent_observations
            .iter()
            .filter_map(|(generation, pending)| (pending.session == session).then_some(*generation))
            .collect();
        for generation in generations {
            self.fail_agent_observation(generation, code, message);
        }
    }

    fn collect_agent_semantic_results(&mut self) {
        let completed = self
            .semantic_runner
            .as_ref()
            .map(semantic::Runner::take_completed)
            .unwrap_or_default();
        for completed in completed {
            if let Some(pending) = self.agent_semantics.get_mut(&completed.generation)
                && !pending.cancelled
            {
                pending.result = Some(completed.result);
            }
        }
    }

    /// Releases every semantic outcome at one manager-owned fixed deadline.
    fn finish_agent_semantic(&mut self, generation: u32) {
        self.collect_agent_semantic_results();
        let Some(pending) = self.agent_semantics.remove(&generation) else {
            return;
        };
        let _ = self.runtime_timer.cancel_agent_semantic(generation);
        if pending.result.is_none()
            && let Some(runner) = self.semantic_runner.as_ref()
        {
            runner.cancel(generation);
        }
        if let Err(error) = self.agent_state.authorize(pending.session, &pending.call) {
            self.send_agent_response(
                pending.session,
                pending.request,
                pending.tool,
                AgentOutcome::Error { error },
            );
            return;
        }
        let descriptor = self.agent_state.descriptor(
            pending.session,
            pending.target,
            &self.clients,
            &self.outputs,
            self,
        );
        let Some(descriptor) = descriptor else {
            self.send_agent_response(
                pending.session,
                pending.request,
                pending.tool,
                AgentOutcome::Error {
                    error: AgentError::no_such_client(),
                },
            );
            return;
        };
        let still_correlated = !descriptor.redacted
            && !pending.cancelled
            && descriptor.generation == pending.client_generation
            && pending.pid != 0
            && xres_local_client_pid(&self.connection, window_id(pending.target))
                == Some(pending.pid);
        let matched = match pending.result {
            Some(semantic::Result::Matched(matched)) if still_correlated => matched,
            Some(semantic::Result::Matched(_)) | Some(semantic::Result::Unavailable) | None => {
                self.send_agent_response(
                    pending.session,
                    pending.request,
                    pending.tool,
                    AgentOutcome::Error {
                        error: AgentError::semantic_unavailable(),
                    },
                );
                return;
            }
        };
        let key = (pending.session, pending.target);
        let outcome = match pending.call {
            nobox_agent_wire::Call::ClientSemanticRoot { .. } => {
                if !matched.nodes.is_empty() || matched.next_offset.is_some() {
                    return self.send_agent_response(
                        pending.session,
                        pending.request,
                        pending.tool,
                        AgentOutcome::Error {
                            error: AgentError::semantic_unavailable(),
                        },
                    );
                }
                let tree_generation = self
                    .agent_semantic_trees
                    .get(&key)
                    .map_or(nobox_agent_wire::TreeGeneration::FIRST, |tree| {
                        tree.generation.next()
                    });
                let tree = AgentSemanticTree::new(tree_generation, matched.root.id);
                let handle = nobox_agent_wire::SemanticNodeHandle {
                    tree: tree_generation,
                    node: nobox_agent_wire::SemanticNodeId::new(1),
                };
                let page = nobox_agent_wire::SemanticTreePage {
                    client: agent_client_id(pending.target),
                    generation: descriptor.generation,
                    tree_generation,
                    root: handle,
                    nodes: vec![nobox_agent_wire::SemanticNode {
                        handle,
                        parent: None,
                        depth: 0,
                        role: matched.root.role,
                        name: matched.root.name,
                        description: None,
                        value: None,
                        states: matched.root.states,
                        bounds: Some(matched.root.bounds),
                        child_count: matched.root.child_count,
                        relations: Vec::new(),
                    }],
                    continuation: None,
                };
                self.agent_semantic_trees.insert(key, tree);
                AgentOutcome::Ok {
                    reply: AgentReply::SemanticTree { page },
                }
            }
            nobox_agent_wire::Call::ClientSemanticTree { .. } => {
                let Some(projection) = pending.projection else {
                    return self.send_agent_response(
                        pending.session,
                        pending.request,
                        pending.tool,
                        AgentOutcome::Error {
                            error: AgentError::semantic_unavailable(),
                        },
                    );
                };
                let Some(tree) = self.agent_semantic_trees.get_mut(&key) else {
                    return self.send_agent_response(
                        pending.session,
                        pending.request,
                        pending.tool,
                        AgentOutcome::Error {
                            error: AgentError::semantic_unavailable(),
                        },
                    );
                };
                if tree.generation != projection.tree_generation {
                    AgentOutcome::Error {
                        error: AgentError::stale_tree(tree.generation),
                    }
                } else if tree.root != matched.root.id {
                    let generation = tree.generation.next();
                    *tree = AgentSemanticTree::new(generation, matched.root.id);
                    AgentOutcome::Error {
                        error: AgentError::stale_tree(generation),
                    }
                } else if !valid_semantic_projection(projection, &matched) {
                    AgentOutcome::Error {
                        error: AgentError::semantic_unavailable(),
                    }
                } else {
                    let Some(root_node) = tree.public_by_internal.get(&projection.root).copied()
                    else {
                        return self.send_agent_response(
                            pending.session,
                            pending.request,
                            pending.tool,
                            AgentOutcome::Error {
                                error: AgentError::semantic_unavailable(),
                            },
                        );
                    };
                    let mut page_nodes = Vec::with_capacity(matched.nodes.len());
                    let mut valid = true;
                    for node in matched.nodes {
                        let parent = node.parent.and_then(|parent| {
                            tree.public_by_internal.get(&parent).copied().map(|node| {
                                nobox_agent_wire::SemanticNodeHandle {
                                    tree: tree.generation,
                                    node,
                                }
                            })
                        });
                        if node.parent.is_some() && parent.is_none() {
                            valid = false;
                            break;
                        }
                        let handle = nobox_agent_wire::SemanticNodeHandle {
                            tree: tree.generation,
                            node: tree.public_id(node.id),
                        };
                        page_nodes.push(nobox_agent_wire::SemanticNode {
                            handle,
                            parent,
                            depth: node.depth,
                            role: node.role,
                            name: node.name,
                            description: None,
                            value: None,
                            states: node.states,
                            bounds: node.bounds,
                            child_count: node.child_count,
                            relations: Vec::new(),
                        });
                    }
                    if !valid {
                        AgentOutcome::Error {
                            error: AgentError::semantic_unavailable(),
                        }
                    } else {
                        if let Some(source) = projection.source_continuation {
                            tree.continuations.remove(&source);
                        }
                        let continuation = matched.next_offset.map(|offset| {
                            tree.issue_continuation(AgentSemanticCursor::Tree {
                                root: projection.root,
                                offset,
                                max_depth: projection.max_depth,
                            })
                        });
                        let root = nobox_agent_wire::SemanticNodeHandle {
                            tree: tree.generation,
                            node: root_node,
                        };
                        AgentOutcome::Ok {
                            reply: AgentReply::SemanticTree {
                                page: nobox_agent_wire::SemanticTreePage {
                                    client: agent_client_id(pending.target),
                                    generation: descriptor.generation,
                                    tree_generation: tree.generation,
                                    root,
                                    nodes: page_nodes,
                                    continuation,
                                },
                            },
                        }
                    }
                }
            }
            nobox_agent_wire::Call::ClientSemanticFind { .. } => {
                let Some(search) = pending.search.as_ref() else {
                    return self.send_agent_response(
                        pending.session,
                        pending.request,
                        pending.tool,
                        AgentOutcome::Error {
                            error: AgentError::semantic_unavailable(),
                        },
                    );
                };
                let Some(tree) = self.agent_semantic_trees.get_mut(&key) else {
                    return self.send_agent_response(
                        pending.session,
                        pending.request,
                        pending.tool,
                        AgentOutcome::Error {
                            error: AgentError::semantic_unavailable(),
                        },
                    );
                };
                if tree.generation != search.tree_generation {
                    AgentOutcome::Error {
                        error: AgentError::stale_tree(tree.generation),
                    }
                } else if tree.root != matched.root.id {
                    let generation = tree.generation.next();
                    *tree = AgentSemanticTree::new(generation, matched.root.id);
                    AgentOutcome::Error {
                        error: AgentError::stale_tree(generation),
                    }
                } else if !valid_semantic_search(search, &matched) {
                    AgentOutcome::Error {
                        error: AgentError::semantic_unavailable(),
                    }
                } else {
                    let mut matches = Vec::with_capacity(matched.nodes.len());
                    for node in matched.nodes {
                        let parent = node.parent.and_then(|parent| {
                            tree.public_by_internal.get(&parent).copied().map(|node| {
                                nobox_agent_wire::SemanticNodeHandle {
                                    tree: tree.generation,
                                    node,
                                }
                            })
                        });
                        let handle = nobox_agent_wire::SemanticNodeHandle {
                            tree: tree.generation,
                            node: tree.public_id(node.id),
                        };
                        matches.push(nobox_agent_wire::SemanticNode {
                            handle,
                            parent,
                            depth: node.depth,
                            role: node.role,
                            name: node.name,
                            description: None,
                            value: None,
                            states: node.states,
                            bounds: node.bounds,
                            child_count: node.child_count,
                            relations: Vec::new(),
                        });
                    }
                    if let Some(source) = search.source_continuation {
                        tree.continuations.remove(&source);
                    }
                    let continuation = matched.next_offset.map(|offset| {
                        tree.issue_continuation(AgentSemanticCursor::Search {
                            offset,
                            query: search.query.clone(),
                        })
                    });
                    AgentOutcome::Ok {
                        reply: AgentReply::SemanticMatches {
                            page: nobox_agent_wire::SemanticSearchPage {
                                client: agent_client_id(pending.target),
                                generation: descriptor.generation,
                                tree_generation: tree.generation,
                                matches,
                                continuation,
                            },
                        },
                    }
                }
            }
            _ => unreachable!("only semantic calls become semantic pending operations"),
        };
        self.send_agent_response(pending.session, pending.request, pending.tool, outcome);
    }

    fn cancel_agent_semantic(&mut self, generation: u32) {
        let Some(pending) = self.agent_semantics.get_mut(&generation) else {
            return;
        };
        pending.cancelled = true;
        pending.result = Some(semantic::Result::Unavailable);
        if let Some(runner) = self.semantic_runner.as_ref() {
            runner.cancel(generation);
        }
    }

    fn fail_session_semantics(
        &mut self,
        session: AgentSessionId,
        code: AgentErrorCode,
        message: &str,
    ) {
        let generations = self
            .agent_semantics
            .iter()
            .filter_map(|(generation, pending)| (pending.session == session).then_some(*generation))
            .collect::<Vec<_>>();
        for generation in generations {
            let Some(pending) = self.agent_semantics.remove(&generation) else {
                continue;
            };
            let _ = self.runtime_timer.cancel_agent_semantic(generation);
            if let Some(runner) = self.semantic_runner.as_ref() {
                runner.cancel(generation);
            }
            self.send_agent_response(
                pending.session,
                pending.request,
                pending.tool,
                AgentOutcome::Error {
                    error: AgentError::new(code, message),
                },
            );
        }
    }

    /// Decides whether an arriving input event was the human's or the
    /// manager's own injection.
    ///
    /// Suppression keys on provenance rather than arrival, so an agent's own
    /// input coming back through the server can never suppress the agent.
    fn note_input_provenance(&mut self, event: &Event) {
        if self.agent_seat.is_none() {
            return;
        }
        let (type_, detail, kind) = match event {
            // Raw events describe the devices themselves, so when they are
            // available they are the only source considered: counting the same
            // input twice would let the manager mistake its own injection for
            // the human.
            Event::XinputRawKeyPress(event) => (
                KEY_PRESS_EVENT_TYPE,
                u8::try_from(event.detail).unwrap_or_default(),
                nobox_agent_wire::HumanActivityKind::Keyboard,
            ),
            Event::XinputRawKeyRelease(event) => (
                KEY_RELEASE_EVENT_TYPE,
                u8::try_from(event.detail).unwrap_or_default(),
                nobox_agent_wire::HumanActivityKind::Keyboard,
            ),
            Event::XinputRawButtonPress(event) => (
                BUTTON_PRESS_EVENT_TYPE,
                u8::try_from(event.detail).unwrap_or_default(),
                nobox_agent_wire::HumanActivityKind::Pointer,
            ),
            Event::XinputRawButtonRelease(event) => (
                BUTTON_RELEASE_EVENT_TYPE,
                u8::try_from(event.detail).unwrap_or_default(),
                nobox_agent_wire::HumanActivityKind::Pointer,
            ),
            Event::XinputRawMotion(_) => (
                MOTION_NOTIFY_EVENT_TYPE,
                0,
                nobox_agent_wire::HumanActivityKind::Pointer,
            ),
            Event::KeyPress(event)
                if !self.raw_input_selected && event.response_type & 0x80 == 0 =>
            {
                (
                    KEY_PRESS_EVENT_TYPE,
                    event.detail,
                    nobox_agent_wire::HumanActivityKind::Keyboard,
                )
            }
            Event::ButtonPress(event)
                if !self.raw_input_selected && event.response_type & 0x80 == 0 =>
            {
                (
                    BUTTON_PRESS_EVENT_TYPE,
                    event.detail,
                    nobox_agent_wire::HumanActivityKind::Pointer,
                )
            }
            _ => return,
        };
        if self.claim_injection(type_, detail) {
            return;
        }
        self.note_human_activity(kind);
    }

    /// Returns whether the human acted recently enough to keep agent input out.
    fn agent_input_suppressed(&self) -> bool {
        nobox_core::agent::is_suppressed(
            self.last_human_input.map(|last| last.elapsed()),
            self.config.agent.suppression(),
        )
    }

    /// Translates content-relative coordinates against live geometry.
    fn agent_root_point(&self, client: ClientId, x: i32, y: i32) -> Result<(i16, i16), X11Error> {
        let Some(managed) = self.clients.get(client) else {
            return Err(X11Error::AgentInput("the target window is gone".to_owned()));
        };
        let geometry = managed.geometry;
        if !nobox_agent_wire::Rect::new(geometry.x, geometry.y, geometry.width, geometry.height)
            .contains_relative(x, y)
        {
            return Err(X11Error::AgentInput(
                "the point is outside the window's content area".to_owned(),
            ));
        }
        let root_x = geometry.x.saturating_add(x);
        let root_y = geometry.y.saturating_add(y);
        let (Ok(root_x), Ok(root_y)) = (i16::try_from(root_x), i16::try_from(root_y)) else {
            return Err(X11Error::AgentInput("the point is off-screen".to_owned()));
        };
        Ok((root_x, root_y))
    }

    /// Injects one pointer action at a point inside a window.
    fn agent_inject_pointer(
        &mut self,
        client: ClientId,
        x: i32,
        y: i32,
        action: nobox_agent_wire::PointerAction,
        button: Option<nobox_agent_wire::PointerButton>,
    ) -> Result<(), X11Error> {
        let (root_x, root_y) = self.agent_root_point(client, x, y)?;
        self.require_agent_pointer_owner(client, root_x, root_y)?;
        self.fake_input(MOTION_NOTIFY_EVENT_TYPE, 0, root_x, root_y)?;
        let detail = button.map(agent_pointer_button);
        match action {
            nobox_agent_wire::PointerAction::Move => {}
            nobox_agent_wire::PointerAction::Press => {
                self.fake_button(detail, true)?;
            }
            nobox_agent_wire::PointerAction::Release => {
                self.fake_button(detail, false)?;
            }
            nobox_agent_wire::PointerAction::Click | nobox_agent_wire::PointerAction::Scroll => {
                self.fake_button(detail, true)?;
                self.fake_button(detail, false)?;
            }
            nobox_agent_wire::PointerAction::DoubleClick => {
                for _ in 0..2 {
                    self.fake_button(detail, true)?;
                    self.fake_button(detail, false)?;
                }
            }
        }
        self.connection.flush()?;
        Ok(())
    }

    /// Refuses a pointer destination whose live X11 input region belongs to a
    /// different top-level client.
    ///
    /// Client captures can intentionally read target-owned Composite storage
    /// through a covering dialog. That makes capture pixels unsuitable as
    /// proof that a click will still reach the captured client. Inspecting the
    /// root's current stacking and input shapes immediately before XTEST keeps
    /// the window-addressed input promise honest.
    fn require_agent_pointer_owner(
        &self,
        target: ClientId,
        root_x: i16,
        root_y: i16,
    ) -> Result<(), X11Error> {
        let children = self.connection.query_tree(self.root)?.reply()?.children;
        if children.len() > MAX_AGENT_HIT_TEST_WINDOWS {
            return Err(X11Error::AgentHitTestBound);
        }
        for child in children.into_iter().rev() {
            let attributes = self.connection.get_window_attributes(child)?.reply()?;
            if attributes.map_state != MapState::VIEWABLE {
                continue;
            }
            let translated = self
                .connection
                .translate_coordinates(self.root, child, root_x, root_y)?
                .reply()?;
            let contains = if self
                .shape_version
                .is_some_and(|version| shape_version_at_least(version, (1, 1)))
            {
                let rectangles = self
                    .connection
                    .shape_get_rectangles(child, SK::INPUT)?
                    .reply()?
                    .rectangles;
                if rectangles.len() > MAX_AGENT_HIT_TEST_RECTANGLES {
                    return Err(X11Error::AgentHitTestBound);
                }
                rectangles.iter().any(|rectangle| {
                    x11_rectangle_contains(*rectangle, translated.dst_x, translated.dst_y)
                })
            } else {
                let geometry = self.connection.get_geometry(child)?.reply()?;
                translated.dst_x >= 0
                    && translated.dst_y >= 0
                    && i32::from(translated.dst_x) < i32::from(geometry.width)
                    && i32::from(translated.dst_y) < i32::from(geometry.height)
            };
            if !contains {
                continue;
            }
            return if self.focus_client_for_window(child)? == Some(target) {
                Ok(())
            } else {
                Err(X11Error::AgentTargetChanged)
            };
        }
        Err(X11Error::AgentTargetChanged)
    }

    /// Injects one key, holding the requested modifiers around it.
    fn agent_inject_key(
        &mut self,
        key: &str,
        action: nobox_agent_wire::KeyAction,
        modifiers: &[nobox_agent_wire::Modifier],
    ) -> Result<(), X11Error> {
        let keycode = self
            .agent_keycode_for_symbol(key)
            .ok_or_else(|| X11Error::AgentInput(format!("no key named {key} on this layout")))?;
        let held: Vec<u8> = modifiers
            .iter()
            .map(|modifier| {
                self.agent_modifier_keycode(*modifier).ok_or_else(|| {
                    X11Error::AgentInput(format!("no {modifier:?} key on this layout"))
                })
            })
            .collect::<Result<_, _>>()?;
        for keycode in &held {
            self.fake_key(*keycode, true)?;
        }
        match action {
            nobox_agent_wire::KeyAction::Press => self.fake_key(keycode, true)?,
            nobox_agent_wire::KeyAction::Release => self.fake_key(keycode, false)?,
            nobox_agent_wire::KeyAction::Tap => {
                self.fake_key(keycode, true)?;
                self.fake_key(keycode, false)?;
            }
        }
        for keycode in held.iter().rev() {
            self.fake_key(*keycode, false)?;
        }
        self.connection.flush()?;
        Ok(())
    }

    /// Resolves the whole string before any part of it can be injected.
    ///
    /// Text available in the current keymap keeps the paced keystroke path.
    /// Otherwise, exact printable UTF-8 uses a temporary selection offer that
    /// is scoped to the target's X11 connection.
    fn agent_text_plan(&self, target: ClientId, text: &str) -> Result<AgentTextPlan, X11Error> {
        let layout = self.keyboard_layout.as_ref().ok_or_else(|| {
            X11Error::AgentInput("the current keyboard layout is unavailable".to_owned())
        })?;
        let strokes = plan_agent_text(
            layout,
            self.agent_modifier_keycode(nobox_agent_wire::Modifier::Shift),
            self.agent_modifier_keycode(nobox_agent_wire::Modifier::AltGr),
            text,
        );
        if let Ok(strokes) = strokes
            && strokes.len() <= MAX_PACED_TEXT_SCALARS
        {
            return Ok(AgentTextPlan::Strokes(strokes));
        }
        if let Some((index, character)) = text
            .chars()
            .enumerate()
            .find(|(_, character)| character.is_control() && !matches!(character, '\n' | '\t'))
        {
            return Err(X11Error::AgentInput(format!(
                "character {} (U+{:04X}) is not printable; exact text also accepts newline and tab",
                index + 1,
                u32::from(character)
            )));
        }
        let client_base =
            xres_client_base(&self.connection, window_id(target)).ok_or_else(|| {
                X11Error::AgentInput(
                    "the target's X11 client identity is unavailable for exact text".to_owned(),
                )
            })?;
        let key = self
            .agent_keycode_for_symbol("v")
            .ok_or_else(|| X11Error::AgentInput("no key named v on this layout".to_owned()))?;
        let control = self
            .agent_modifier_keycode(nobox_agent_wire::Modifier::Control)
            .ok_or_else(|| X11Error::AgentInput("no Control key on this layout".to_owned()))?;
        Ok(AgentTextPlan::Transfer {
            text: text.to_owned(),
            client_base,
            paste: AgentPasteChord { control, key },
        })
    }

    /// Claims `CLIPBOARD` for one exact-text request and sends Control+V.
    fn begin_agent_text_transfer(&mut self, paste: AgentPasteChord) -> Result<(), X11Error> {
        self.connection
            .set_selection_owner(self.support_window, self.atoms.CLIPBOARD, CURRENT_TIME)?
            .check()?;
        let owner = self
            .connection
            .get_selection_owner(self.atoms.CLIPBOARD)?
            .reply()?
            .owner;
        if owner != self.support_window {
            return Err(X11Error::AgentInput(
                "could not claim the clipboard for exact text".to_owned(),
            ));
        }
        self.fake_key(paste.control, true)?;
        self.fake_key(paste.key, true)?;
        self.fake_key(paste.key, false)?;
        self.fake_key(paste.control, false)?;
        self.connection.flush()?;
        Ok(())
    }

    /// Releases only the request-local clipboard ownership that is still ours.
    fn release_agent_text_selection(&self) -> Result<(), X11Error> {
        let owner = self
            .connection
            .get_selection_owner(self.atoms.CLIPBOARD)?
            .reply()?
            .owner;
        if owner == self.support_window {
            self.connection
                .set_selection_owner(NONE, self.atoms.CLIPBOARD, CURRENT_TIME)?
                .check()?;
            self.connection.flush()?;
        }
        Ok(())
    }

    /// Emits one text stroke whose complete plan was validated before typing.
    fn agent_inject_text_stroke(&mut self, stroke: AgentTextStroke) -> Result<(), X11Error> {
        for modifier in stroke.modifiers.into_iter().flatten() {
            self.fake_key(modifier, true)?;
        }
        self.fake_key(stroke.keycode, true)?;
        self.fake_key(stroke.keycode, false)?;
        for modifier in stroke.modifiers.into_iter().rev().flatten() {
            self.fake_key(modifier, false)?;
        }
        self.connection.flush()?;
        Ok(())
    }

    fn fake_button(&mut self, detail: Option<u8>, press: bool) -> Result<(), X11Error> {
        let detail = detail.ok_or_else(|| {
            X11Error::AgentInput("this pointer action requires a button".to_owned())
        })?;
        let type_ = if press {
            BUTTON_PRESS_EVENT_TYPE
        } else {
            BUTTON_RELEASE_EVENT_TYPE
        };
        self.fake_input(type_, detail, 0, 0)
    }

    fn fake_key(&mut self, keycode: u8, press: bool) -> Result<(), X11Error> {
        let type_ = if press {
            KEY_PRESS_EVENT_TYPE
        } else {
            KEY_RELEASE_EVENT_TYPE
        };
        self.fake_input(type_, keycode, 0, 0)
    }

    /// Synthesizes one input event and records that the manager originated it.
    ///
    /// Provenance is recorded before the event exists, so the manager can never
    /// mistake its own injection for fresh human activity when it comes back
    /// through the server.
    fn fake_input(
        &mut self,
        type_: u8,
        detail: u8,
        root_x: i16,
        root_y: i16,
    ) -> Result<(), X11Error> {
        self.remember_injection(type_, detail);
        self.connection.xtest_fake_input(
            type_,
            detail,
            CURRENT_TIME,
            self.root,
            root_x,
            root_y,
            0,
        )?;
        Ok(())
    }

    fn remember_injection(&mut self, type_: u8, detail: u8) {
        let now = Instant::now();
        self.agent_injections
            .retain(|injection| injection.expires > now);
        if self.agent_injections.len() >= MAX_TRACKED_INJECTIONS {
            self.agent_injections.pop_front();
        }
        self.agent_injections.push_back(InjectedInput {
            type_,
            detail,
            expires: now + INJECTION_PROVENANCE_WINDOW,
        });
    }

    /// Returns whether an arriving event is one the manager injected itself.
    fn claim_injection(&mut self, type_: u8, detail: u8) -> bool {
        let now = Instant::now();
        self.agent_injections
            .retain(|injection| injection.expires > now);
        let Some(index) = self
            .agent_injections
            .iter()
            .position(|injection| injection.type_ == type_ && injection.detail == detail)
        else {
            return false;
        };
        self.agent_injections.remove(index);
        true
    }

    /// Records that the human used an input device.
    ///
    /// Only events the manager did not originate reach here, so agent input can
    /// never suppress itself.
    fn note_human_activity(&mut self, kind: nobox_agent_wire::HumanActivityKind) {
        let now = Instant::now();
        self.last_human_input = Some(now);
        self.interrupt_pending_agent_work();
        let announce = self
            .last_human_event
            .is_none_or(|last| now.duration_since(last) >= HUMAN_ACTIVITY_INTERVAL);
        if !announce {
            return;
        }
        self.last_human_event = Some(now);
        self.emit_agent_event(
            nobox_agent_wire::EventKind::HumanActivity,
            None,
            // Only that it happened, never what was typed or where.
            |_, _| Some(nobox_agent_wire::Event::HumanActivity { kind }),
        );
    }

    /// Freezes every agent session, or resumes them if they are already frozen.
    ///
    /// This runs in the manager's own key path, ahead of any agent traffic, so
    /// it works while a session is flooding the socket.
    fn toggle_agent_freeze(&mut self) -> Result<(), X11Error> {
        let (changed, change) = if self.agent_state.any_frozen() {
            (
                self.agent_state.resume_all(),
                nobox_agent_wire::SessionChange::Resumed,
            )
        } else {
            (
                self.agent_state.freeze_all(),
                nobox_agent_wire::SessionChange::Frozen,
            )
        };
        if changed.is_empty() {
            info!("agent kill chord pressed with no sessions to freeze");
            return Ok(());
        }
        if change == nobox_agent_wire::SessionChange::Frozen {
            for session in &changed {
                self.fail_session_text(
                    *session,
                    AgentErrorCode::SessionFrozen,
                    "the agent session was frozen",
                );
                self.fail_session_observations(
                    *session,
                    AgentErrorCode::SessionFrozen,
                    "the agent session was frozen",
                );
                self.fail_session_semantics(
                    *session,
                    AgentErrorCode::SessionFrozen,
                    "the agent session was frozen",
                );
            }
        }
        warn!(
            sessions = changed.len(),
            ?change,
            "agent sessions changed by the kill chord"
        );
        self.emit_agent_event(nobox_agent_wire::EventKind::SessionControl, None, |_, _| {
            Some(nobox_agent_wire::Event::SessionControl { change })
        });
        self.flush_agent_events();
        self.refresh_agent_indicator()
    }

    /// Marks the window currently receiving agent input.
    fn mark_agent_input_target(&mut self, client: ClientId) {
        self.agent_input_target = Some((client, Instant::now() + AGENT_INPUT_MARKER_HOLD));
        if let Err(error) = self
            .refresh_frame_colors(client)
            .and_then(|()| self.connection.flush().map_err(X11Error::from))
        {
            warn!(%error, "could not mark the window receiving agent input");
        }
        if let Err(error) = self.runtime_timer.arm_agent_marker(AGENT_INPUT_MARKER_HOLD) {
            warn!(%error, "could not schedule the agent marker to clear");
        }
    }

    /// Clears the marker once its hold has elapsed.
    fn expire_agent_input_target(&mut self) -> Result<(), X11Error> {
        let Some((_, until)) = self.agent_input_target else {
            return Ok(());
        };
        if Instant::now() < until {
            return Ok(());
        }
        let previous = self.agent_input_target.take();
        if let Some((client, _)) = previous {
            self.refresh_frame_colors(client)?;
        }
        self.connection.flush()?;
        Ok(())
    }

    /// Shows or hides the standing marker that a session holds input or
    /// capture. The protocol offers no way to draw, cover, target, or dismiss
    /// it.
    fn refresh_agent_indicator(&mut self) -> Result<(), X11Error> {
        let wanted = self.agent_seat.is_some() && self.agent_state.any_holds_visible_capability();
        if !wanted {
            if let Some(window) = self.agent_indicator.take() {
                self.connection.destroy_window(window)?;
                self.connection.flush()?;
            }
            return Ok(());
        }
        if self.agent_indicator.is_some() {
            return self.place_agent_indicator();
        }
        let window = self.connection.generate_id()?;
        self.connection
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                0,
                0,
                AGENT_INDICATOR_WIDTH,
                AGENT_INDICATOR_HEIGHT,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .background_pixel(self.decoration_pixels.agent_marker)
                    .override_redirect(1_u32)
                    .event_mask(EventMask::EXPOSURE),
            )?
            .check()?;
        self.connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms._NET_WM_NAME,
            self.atoms.UTF8_STRING,
            b"nobox agent seat",
        )?;
        self.agent_indicator = Some(window);
        self.place_agent_indicator()?;
        self.connection.map_window(window)?;
        self.connection.flush()?;
        Ok(())
    }

    fn place_agent_indicator(&mut self) -> Result<(), X11Error> {
        let Some(window) = self.agent_indicator else {
            return Ok(());
        };
        let output = self.outputs.primary().geometry;
        let x = output
            .x
            .saturating_add(i32::try_from(output.width).unwrap_or(i32::MAX))
            .saturating_sub(i32::from(AGENT_INDICATOR_WIDTH));
        self.connection.configure_window(
            window,
            &ConfigureWindowAux::new()
                .x(x)
                .y(output.y)
                .width(u32::from(AGENT_INDICATOR_WIDTH))
                .height(u32::from(AGENT_INDICATOR_HEIGHT))
                .stack_mode(StackMode::ABOVE),
        )?;
        Ok(())
    }

    fn draw_agent_indicator(&self) -> Result<(), X11Error> {
        let Some(window) = self.agent_indicator else {
            return Ok(());
        };
        let label = if self.agent_state.any_frozen() {
            "agent frozen"
        } else {
            "agent seat"
        };
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new()
                .foreground(self.decoration_pixels.title_text)
                .background(self.decoration_pixels.agent_marker),
        )?;
        self.connection.image_text8(
            window,
            self.title_gc,
            4,
            i16::try_from(AGENT_INDICATOR_HEIGHT).unwrap_or(14) - 4,
            label.as_bytes(),
        )?;
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new().foreground(self.decoration_pixels.title_text),
        )?;
        Ok(())
    }

    fn agent_keycode_for_symbol(&self, name: &str) -> Option<u8> {
        let layout = self.keyboard_layout.as_ref()?;
        let name = canonical_agent_key_name(name);
        keycodes_for_named_symbol(layout.minimum, layout.per_keycode, &layout.keysyms, name)
            .into_iter()
            .next()
    }

    fn agent_modifier_keycode(&self, modifier: nobox_agent_wire::Modifier) -> Option<u8> {
        let mask = match modifier {
            nobox_agent_wire::Modifier::Shift => u16::from(ModMask::SHIFT),
            nobox_agent_wire::Modifier::Control => u16::from(ModMask::CONTROL),
            nobox_agent_wire::Modifier::Alt => u16::from(ModMask::M1),
            nobox_agent_wire::Modifier::Super => u16::from(ModMask::M4),
            nobox_agent_wire::Modifier::AltGr => u16::from(ModMask::M5),
        };
        self.modifier_keycodes
            .iter()
            .find(|(_, candidate)| **candidate == mask)
            .map(|(keycode, _)| *keycode)
    }

    /// Asks the human whether a companion may hold what it asked for.
    ///
    /// The dialog is drawn by the manager on its own override-redirect window
    /// and holds the keyboard while it is up. Nothing in the protocol can
    /// create, cover, target, or dismiss it, and the session waits: no grant
    /// exists until a person answers.
    fn begin_agent_consent(&mut self, pending: PendingConsent) -> Result<(), X11Error> {
        if self.agent_consent.is_some() {
            self.agent_consent_queue.push_back(pending);
            return Ok(());
        }
        let width = AGENT_CONSENT_WIDTH;
        let lines = agent_consent_lines(&pending);
        let height = u16::try_from(lines.len())
            .unwrap_or(6)
            .saturating_mul(AGENT_CONSENT_LINE_HEIGHT)
            .saturating_add(AGENT_CONSENT_LINE_HEIGHT);
        let output = self.outputs.primary().geometry;
        let x = output.x + (i32::try_from(output.width).unwrap_or(0) - i32::from(width)) / 2;
        let y = output.y + (i32::try_from(output.height).unwrap_or(0) - i32::from(height)) / 3;
        let window = self.connection.generate_id()?;
        self.connection
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                i16::try_from(x).unwrap_or(0),
                i16::try_from(y).unwrap_or(0),
                width,
                height,
                x_u16(self.config.theme.border_width.clamp(1, 8)),
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .background_pixel(self.decoration_pixels.active_titlebar)
                    .border_pixel(self.decoration_pixels.agent_marker)
                    .override_redirect(1_u32)
                    .save_under(1_u32)
                    .event_mask(EventMask::EXPOSURE),
            )?
            .check()?;
        self.connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms._NET_WM_NAME,
            self.atoms.UTF8_STRING,
            b"nobox agent consent",
        )?;
        self.connection.map_window(window)?;
        self.connection.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )?;
        let status = self
            .connection
            .grab_keyboard(
                false,
                self.root,
                CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )?
            .reply()?
            .status;
        if status != GrabStatus::SUCCESS {
            warn!(
                ?status,
                "could not hold the keyboard for the agent consent dialog"
            );
        }
        self.connection.flush()?;
        info!(
            session = %pending.session,
            harness = %pending.hello.harness,
            "asking the human about an agent session"
        );
        self.agent_consent = Some(ActiveConsent {
            pending,
            window,
            lines,
        });
        Ok(())
    }

    fn draw_agent_consent(&self) -> Result<(), X11Error> {
        let Some(consent) = self.agent_consent.as_ref() else {
            return Ok(());
        };
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new()
                .foreground(self.decoration_pixels.title_text)
                .background(self.decoration_pixels.active_titlebar),
        )?;
        for (index, line) in consent.lines.iter().enumerate() {
            let baseline = AGENT_CONSENT_LINE_HEIGHT
                .saturating_mul(u16::try_from(index).unwrap_or(0).saturating_add(1));
            self.connection.image_text8(
                consent.window,
                self.title_gc,
                8,
                i16::try_from(baseline).unwrap_or(i16::MAX),
                &title_text_bytes(line, 96),
            )?;
        }
        self.connection.flush()?;
        Ok(())
    }

    /// Handles a key while the consent dialog is up.
    fn agent_consent_key(&mut self, event: &KeyPressEvent) -> Result<bool, X11Error> {
        if self.agent_consent.is_none() {
            return Ok(false);
        }
        let answer = if self.escape_keycodes.contains(&event.detail) {
            Some(ConsentAnswer::Deny)
        } else {
            match self.agent_consent_symbol(event.detail) {
                Some('y') => Some(ConsentAnswer::Once),
                Some('p') => Some(ConsentAnswer::Persist),
                Some('n') => Some(ConsentAnswer::Deny),
                _ => None,
            }
        };
        let Some(answer) = answer else {
            return Ok(true);
        };
        self.finish_agent_consent(answer, event.time)?;
        Ok(true)
    }

    fn agent_consent_symbol(&self, keycode: u8) -> Option<char> {
        let layout = self.keyboard_layout.as_ref()?;
        let per = usize::from(layout.per_keycode);
        let index = usize::from(keycode).checked_sub(usize::from(layout.minimum))?;
        let symbol = layout.keysyms.get(index * per).copied()?;
        char::from_u32(symbol).filter(char::is_ascii_alphabetic)
    }

    /// Applies the human's answer and lets the session know.
    fn finish_agent_consent(
        &mut self,
        answer: ConsentAnswer,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        let Some(consent) = self.agent_consent.take() else {
            return Ok(());
        };
        self.connection.destroy_window(consent.window)?;
        self.connection.ungrab_keyboard(timestamp)?;
        self.connection.flush()?;
        let pending = consent.pending;
        let atoms =
            match answer {
                ConsentAnswer::Deny => AgentCapabilities::EMPTY,
                ConsentAnswer::Once | ConsentAnswer::Persist => pending
                    .hello
                    .requested
                    .iter()
                    .fold(AgentCapabilities::EMPTY, |set, bundle| {
                        set.union(AgentCapabilities::from_iter_atoms(
                            bundle.atoms().iter().copied(),
                        ))
                    }),
            };
        if matches!(answer, ConsentAnswer::Persist) && !atoms.is_empty() {
            self.persist_agent_grant(&pending, atoms);
        }
        info!(
            session = %pending.session,
            harness = %pending.hello.harness,
            ?answer,
            granted = ?atoms.atoms(),
            "the human answered an agent consent request"
        );
        if !atoms.is_empty() {
            self.agent_consented.insert(pending.session);
        }
        self.complete_agent_greeting(&pending, AgentGrant::new(atoms));
        if let Some(next) = self.agent_consent_queue.pop_front() {
            self.begin_agent_consent(next)?;
        }
        Ok(())
    }

    /// Writes a consented grant into the user's configuration file, and into
    /// the configuration this manager is running on.
    ///
    /// Both halves matter. The file is what survives a restart; the running
    /// copy is what the next connection is actually checked against. Writing
    /// only the file means "allow and remember" behaves exactly like "allow
    /// once" until someone reloads, so a person who answered the question
    /// permanently is asked it again on the very next connection.
    fn persist_agent_grant(&mut self, pending: &PendingConsent, atoms: AgentCapabilities) {
        let Some(executable) = pending.executable.as_deref() else {
            warn!("cannot persist a grant for a peer whose executable is unknown");
            return;
        };
        let capabilities: Vec<String> = atoms
            .atoms()
            .into_iter()
            .map(|atom| atom.as_str().to_owned())
            .collect();
        let path = match nobox_config::config_path() {
            Ok(path) => path,
            Err(error) => {
                warn!(%error, "cannot find a configuration file to store the grant in");
                return;
            }
        };
        let stored = nobox_config::ConfigDocument::load(&path).and_then(|mut document| {
            document.append_agent_grant(
                &pending.hello.harness,
                executable,
                Some(pending.uid),
                &capabilities,
            )?;
            document.save(&path)?;
            Ok(())
        });
        match stored {
            Ok(()) => info!(path = %path.display(), "stored an agent grant"),
            Err(error) => {
                warn!(%error, "could not store the agent grant");
                return;
            }
        }
        // The file now says this companion is allowed. Make the configuration
        // this manager is deciding against say the same thing, so the next
        // connection matches the stored grant instead of asking again. The
        // running copy and the file stay in step: nothing is remembered here
        // that was not also written.
        if self.config.agent.grants.len() >= nobox_config::MAX_AGENT_GRANTS {
            warn!(
                limit = nobox_config::MAX_AGENT_GRANTS,
                "stored the grant but kept the running configuration unchanged: too many grants"
            );
            return;
        }
        self.config.agent.grants.push(nobox_config::AgentGrant {
            label: pending.hello.harness.clone(),
            executable: executable.to_path_buf(),
            uid: Some(pending.uid),
            capabilities: atoms
                .atoms()
                .into_iter()
                .map(nobox_config::GrantedCapability::Atom)
                .collect(),
            scope: None,
        });
    }

    /// Re-evaluates every live session's grant against a new configuration.
    ///
    /// A grant the human took away must stop working now rather than at the
    /// next connection. A session the human consented to interactively keeps
    /// what it was given: that answer was about this session, and no edit to
    /// the stored grants was aimed at it.
    fn reapply_agent_grants(&mut self, config: &Config) {
        let sessions: Vec<AgentSessionId> = self
            .agent_state
            .sessions()
            .map(|(session, _)| session)
            .collect();
        let mut revoked = Vec::new();
        for session in sessions {
            if self.agent_consented.contains(&session) {
                continue;
            }
            let peer = self
                .agent_seat
                .as_ref()
                .and_then(|seat| seat.peer(session))
                .map(|peer| (peer.uid, peer.executable.clone()));
            let Some((uid, executable)) = peer else {
                continue;
            };
            let configured = config.agent.grant_for(executable.as_deref(), uid).cloned();
            let grant = match &configured {
                Some(configured) if configured.scope.is_some() => {
                    AgentGrant::scoped(configured.capabilities())
                }
                Some(configured) => AgentGrant::new(configured.capabilities()),
                None => AgentGrant::denied(),
            };
            if grant.capabilities() == AgentCapabilities::EMPTY {
                revoked.push(session);
            }
            match configured.as_ref().and_then(|grant| grant.scope.clone()) {
                Some(scope) => {
                    self.agent_scopes.insert(session, scope);
                }
                None => {
                    self.agent_scopes.remove(&session);
                }
            }
            self.agent_state.set_grant(session, grant);
        }
        // A new scope needs its membership rebuilt against every client.
        for client in self.clients.management_order().collect::<Vec<_>>() {
            self.register_agent_client(client);
        }
        if revoked.is_empty() {
            return;
        }
        warn!(
            sessions = revoked.len(),
            "agent grants revoked by configuration"
        );
        for session in revoked {
            self.agent_state
                .set_status(session, nobox_core::agent::SessionStatus::Revoked);
            self.fail_session_text(
                session,
                AgentErrorCode::SessionRevoked,
                "the agent session grant was revoked",
            );
            self.fail_session_observations(
                session,
                AgentErrorCode::SessionRevoked,
                "the agent session grant was revoked",
            );
            self.fail_session_semantics(
                session,
                AgentErrorCode::SessionRevoked,
                "the agent session grant was revoked",
            );
        }
        self.emit_agent_event(nobox_agent_wire::EventKind::SessionControl, None, |_, _| {
            Some(nobox_agent_wire::Event::SessionControl {
                change: nobox_agent_wire::SessionChange::Revoked,
            })
        });
        self.flush_agent_events();
    }

    /// Starts an application from the desktop-entry catalog.
    ///
    /// Only catalog identifiers are expressible: there is no shell string in
    /// this protocol, and the entry's own Exec expansion is what runs. A
    /// desktop entry still runs code, which is why launching is bounded by an
    /// explicit policy rather than by the catalog alone.
    fn agent_launch(
        &mut self,
        session: AgentSessionId,
        desktop_entry: &str,
        uris: &[String],
    ) -> AgentOutcome {
        let Some(application) = self.application_catalog.find(desktop_entry).cloned() else {
            return AgentOutcome::Error {
                error: AgentError::new(AgentErrorCode::NoSuchTarget, "no such desktop entry"),
            };
        };
        if !self
            .config
            .agent
            .launch
            .allows(desktop_entry, application.user_installed)
        {
            info!(
                session = %session,
                desktop_entry,
                user_installed = application.user_installed,
                "refusing an agent launch outside the configured policy"
            );
            return AgentOutcome::Error {
                error: AgentError::new(
                    AgentErrorCode::LaunchDenied,
                    "launch policy does not allow this application",
                ),
            };
        }
        if !uris.is_empty() {
            // Arguments would be expanded by the entry's own Exec field, which
            // is not implemented yet; refusing beats passing them somewhere
            // unexpected.
            return AgentOutcome::Error {
                error: AgentError::new(
                    AgentErrorCode::Unsupported,
                    "this manager cannot pass arguments to a desktop entry yet",
                ),
            };
        }
        // Correlation is always requested, whatever the entry asks for: the
        // token is how launch-and-identify stays one round trip.
        let notification = StartupNotification {
            name: Some(application.name.clone()),
            icon: application.icon.clone(),
            wm_class: application.startup_wm_class.clone(),
        };
        let timestamp = self.last_timestamp;
        let prepared = match self.prepare_execute_command(
            PreparedCommand::Direct(application.command.clone()),
            Some(notification),
            None,
            None,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                return AgentOutcome::Error {
                    error: AgentError::new(AgentErrorCode::Internal, error.to_string()),
                };
            }
        };
        let before: BTreeSet<String> = self.startup_sequences.keys().cloned().collect();
        if let Err(error) = self.execute_prepared(prepared, timestamp) {
            return AgentOutcome::Error {
                error: AgentError::new(AgentErrorCode::Internal, error.to_string()),
            };
        }
        let Some(token) = self
            .startup_sequences
            .keys()
            .find(|id| !before.contains(*id))
            .cloned()
        else {
            return AgentOutcome::Error {
                error: AgentError::new(
                    AgentErrorCode::Internal,
                    "the launch produced no correlation token",
                ),
            };
        };
        info!(session = %session, desktop_entry, token, "agent launched an application");
        self.agent_launch_pending.insert(token.clone());
        AgentOutcome::Ok {
            reply: AgentReply::Launched { launch: token },
        }
    }

    /// Captures one client's pixels.
    ///
    /// Three capabilities live behind one tool because they are three
    /// different promises: seeing a window that is on screen, seeing one that
    /// is covered or on another workspace, and seeing whatever happens to be
    /// in front of it. The last is never granted implicitly.
    fn agent_capture_client(
        &mut self,
        session: AgentSessionId,
        client: nobox_agent_wire::ClientId,
        area: nobox_agent_wire::CaptureArea,
        rect: Option<nobox_agent_wire::Rect>,
        grid: Option<nobox_agent_wire::CaptureGrid>,
        expects: &nobox_agent_wire::Expects,
    ) -> AgentOutcome {
        let target = client_id_from_agent(client);
        if !self.agent_state.perceives(session, target) {
            return AgentOutcome::Error {
                error: AgentError::no_such_client(),
            };
        }
        if matches!(
            self.agent_state.visibility(target),
            AgentClientVisibility::Redacted
        ) {
            return AgentOutcome::Error {
                error: AgentError::denied("this client is redacted; capture is refused"),
            };
        }
        if let Err(error) = self
            .agent_state
            .check_expects(target, expects, &self.clients)
        {
            return AgentOutcome::Error { error };
        }
        let Some(managed) = self.clients.get(target).copied() else {
            return AgentOutcome::Error {
                error: AgentError::no_such_client(),
            };
        };
        let frame = self.frames.get(&target).copied();
        let (drawable, full) = match area {
            nobox_agent_wire::CaptureArea::Content => (window_id(target), managed.geometry),
            nobox_agent_wire::CaptureArea::Frame => match frame {
                Some(frame) => (frame.window, frame.extents.outer_geometry(managed.geometry)),
                None => (window_id(target), managed.geometry),
            },
        };
        // A crop is expressed in the coordinates input takes, which is where
        // the caller read it off a previous capture. Clipping rather than
        // refusing keeps a request near an edge useful; an empty intersection
        // is a mistake worth naming.
        let rectangle = match rect {
            None => full,
            Some(requested) => {
                match clip_capture_rect(full, (managed.geometry.x, managed.geometry.y), requested) {
                    Some(clipped) => clipped,
                    None => {
                        return AgentOutcome::Error {
                            error: AgentError::new(
                                AgentErrorCode::InvalidArgument,
                                "that rectangle lies outside the area being captured",
                            ),
                        };
                    }
                }
            }
        };
        let content_origin = (
            rectangle.x - managed.geometry.x,
            rectangle.y - managed.geometry.y,
        );
        // Reading pixels off the screen returns whatever is in front of the
        // window, so anything the user marked sensitive that overlaps it must
        // not come back through a capture aimed at something else.
        let obstructed = self.agent_capture_obstructed(target, rectangle);
        let off_screen = !geometry_contains(self.root_geometry, rectangle);
        let on_screen = self.clients.is_visible(target) && !managed.iconic;
        if !on_screen {
            // A window that is not mapped has no pixels anywhere: the server
            // frees its contents, and no extension brings them back. Saying so
            // is better than returning something that is not what was asked
            // for.
            return AgentOutcome::Error {
                error: AgentError::new(
                    AgentErrorCode::Unsupported,
                    "this window is not rendered right now; restore it first",
                ),
            };
        }
        let indirect = obstructed || off_screen;
        if indirect {
            let holds = self.agent_state.session(session).is_some_and(|state| {
                state
                    .grant()
                    .capabilities()
                    .holds(nobox_agent_wire::Capability::CaptureClientObscured)
            });
            if !holds {
                return AgentOutcome::Error {
                    error: AgentError::denied(
                        "this window is covered or off-screen, which is a separate capability",
                    ),
                };
            }
            if self.composite_version.is_none() {
                return AgentOutcome::Error {
                    error: AgentError::new(
                        AgentErrorCode::Unsupported,
                        "capturing a covered window needs the Composite extension",
                    ),
                };
            }
        }
        match self.capture_drawable(
            session,
            drawable,
            rectangle,
            (full.x, full.y),
            indirect,
            grid.map(|grid| (grid, content_origin)),
        ) {
            Ok(mut image) => {
                // Say which part of the window these pixels are, in the
                // coordinates a pointer call takes, so a crop can be aimed at
                // without the caller reconstructing the offset itself.
                image.content = Some(nobox_agent_wire::Rect::new(
                    content_origin.0,
                    content_origin.1,
                    rectangle.width,
                    rectangle.height,
                ));
                AgentOutcome::Ok {
                    reply: AgentReply::Capture { image },
                }
            }
            Err(X11Error::AgentInput(message)) => AgentOutcome::Error {
                error: AgentError::new(AgentErrorCode::InvalidArgument, message),
            },
            Err(error) => {
                warn!(session = %session, %error, "an agent capture failed");
                AgentOutcome::Error {
                    error: AgentError::new(AgentErrorCode::Internal, error.to_string()),
                }
            }
        }
    }

    /// Captures an output or one output-local region.
    ///
    /// Full-output capture is permission to see every currently displayed
    /// pixel, so an application scope never makes it safe: it is refused
    /// outright while anything the user marked sensitive overlaps the region.
    fn agent_capture_output(
        &mut self,
        session: AgentSessionId,
        output: nobox_agent_wire::OutputId,
        rect: Option<nobox_agent_wire::Rect>,
    ) -> AgentOutcome {
        let target = OutputId::new(output.raw());
        let Some(geometry) = self
            .outputs
            .outputs()
            .iter()
            .find(|candidate| candidate.id == target)
            .map(|candidate| candidate.geometry)
        else {
            return AgentOutcome::Error {
                error: AgentError::new(AgentErrorCode::NoSuchTarget, "no such output"),
            };
        };
        let rectangle = match rect {
            None => geometry,
            Some(requested) => {
                match clip_capture_rect(geometry, (geometry.x, geometry.y), requested) {
                    Some(clipped) => clipped,
                    None => {
                        return AgentOutcome::Error {
                            error: AgentError::new(
                                AgentErrorCode::InvalidArgument,
                                "that rectangle lies outside the output being captured",
                            ),
                        };
                    }
                }
            }
        };
        if let Some(sensitive) = self.agent_sensitive_client_on(rectangle) {
            debug!(
                client = sensitive.raw(),
                "refusing an output capture while a sensitive window is displayed"
            );
            return AgentOutcome::Error {
                error: AgentError::denied(
                    "a window the user marked sensitive overlaps this output capture",
                ),
            };
        }
        match self.capture_drawable(session, self.root, rectangle, (0, 0), false, None) {
            Ok(image) => AgentOutcome::Ok {
                reply: AgentReply::Capture { image },
            },
            Err(X11Error::AgentInput(message)) => AgentOutcome::Error {
                error: AgentError::new(AgentErrorCode::InvalidArgument, message),
            },
            Err(error) => {
                warn!(%error, "an agent output capture failed");
                AgentOutcome::Error {
                    error: AgentError::new(AgentErrorCode::Internal, error.to_string()),
                }
            }
        }
    }

    /// Returns a visible hidden or redacted client whose frame overlaps a
    /// rectangle.
    fn agent_sensitive_client_on(&self, area: Geometry) -> Option<ClientId> {
        self.clients.stacking().find(|client| {
            !matches!(
                self.agent_state.visibility(*client),
                AgentClientVisibility::Visible
            ) && self.clients.is_visible(*client)
                && self
                    .clients
                    .get(*client)
                    .is_some_and(|managed| !managed.iconic)
                && geometries_overlap(AgentClientDetails::frame(self, *client), area)
        })
    }

    /// Returns whether anything sensitive sits above a client and over it.
    fn agent_capture_obstructed(&self, client: ClientId, area: Geometry) -> bool {
        let order: Vec<ClientId> = self.clients.stacking().collect();
        let Some(position) = order.iter().position(|candidate| *candidate == client) else {
            return false;
        };
        order[position + 1..].iter().any(|above| {
            !matches!(
                self.agent_state.visibility(*above),
                AgentClientVisibility::Visible
            ) && self.clients.is_visible(*above)
                && geometries_overlap(AgentClientDetails::frame(self, *above), area)
        })
    }

    /// Reads pixels and encodes them, optionally through a redirected pixmap
    /// so a covered window yields its own contents rather than what is on top.
    fn capture_drawable(
        &self,
        session: AgentSessionId,
        drawable: Window,
        area: Geometry,
        drawable_origin: (i32, i32),
        redirect: bool,
        grid: Option<(nobox_agent_wire::CaptureGrid, (i32, i32))>,
    ) -> Result<nobox_agent_wire::CaptureImage, X11Error> {
        let pixels = u64::from(area.width) * u64::from(area.height);
        if area.width == 0 || area.height == 0 || pixels > MAX_CAPTURE_PIXELS {
            return Err(X11Error::AgentInput(format!(
                "the capture area is empty or exceeds the {MAX_CAPTURE_PIXELS}-pixel limit; request a smaller rect"
            )));
        }
        let drawable_area = drawable_capture_area(area, drawable_origin)?;
        let source = if redirect {
            self.connection
                .composite_redirect_window(
                    drawable,
                    x11rb::protocol::composite::Redirect::AUTOMATIC,
                )?
                .check()?;
            let pixmap = self.connection.generate_id()?;
            self.connection
                .composite_name_window_pixmap(drawable, pixmap)?
                .check()?;
            Some(pixmap)
        } else {
            None
        };
        let result = self
            .connection
            .get_image(
                x11rb::protocol::xproto::ImageFormat::Z_PIXMAP,
                source.unwrap_or(drawable),
                drawable_area.x,
                drawable_area.y,
                drawable_area.width,
                drawable_area.height,
                !0,
            )
            .map(x11rb::cookie::Cookie::reply);
        if let Some(pixmap) = source {
            self.connection.free_pixmap(pixmap)?;
            self.connection.composite_unredirect_window(
                drawable,
                x11rb::protocol::composite::Redirect::AUTOMATIC,
            )?;
        }
        let reply = result??;
        let data = self.encode_png(
            drawable_area.width,
            drawable_area.height,
            reply.depth,
            &reply.data,
            grid,
        )?;
        Ok(nobox_agent_wire::CaptureImage {
            content: None,
            grid: grid.map(|(grid, origin)| nobox_agent_wire::AppliedCaptureGrid {
                spacing: grid.spacing,
                origin_x: origin.0,
                origin_y: origin.1,
            }),
            format: nobox_agent_wire::ImageFormat::Png,
            width: u32::from(drawable_area.width),
            height: u32::from(drawable_area.height),
            source: agent_rect(area),
            sequence: self.agent_state.sequence(session),
            data: nobox_agent_wire::Base64Bytes::new(data),
        })
    }

    /// Encodes server pixels as PNG, honoring the screen's own channel layout.
    fn encode_png(
        &self,
        width: u16,
        height: u16,
        depth: u8,
        data: &[u8],
        grid: Option<(nobox_agent_wire::CaptureGrid, (i32, i32))>,
    ) -> Result<Vec<u8>, X11Error> {
        let setup = self.connection.setup();
        let bits_per_pixel = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == depth)
            .map_or(32, |format| format.bits_per_pixel);
        if bits_per_pixel != 24 && bits_per_pixel != 32 {
            return Err(X11Error::AgentInput(format!(
                "the manager cannot encode {bits_per_pixel}-bit pixels"
            )));
        }
        let bytes_per_pixel = usize::from(bits_per_pixel / 8);
        let visual = setup.roots[self.screen_index]
            .allowed_depths
            .iter()
            .flat_map(|allowed| allowed.visuals.iter())
            .find(|visual| visual.visual_id == setup.roots[self.screen_index].root_visual);
        let (red_mask, green_mask, blue_mask) = visual
            .map_or((0x00ff_0000, 0x0000_ff00, 0x0000_00ff), |visual| {
                (visual.red_mask, visual.green_mask, visual.blue_mask)
            });
        let stride = usize::from(width) * bytes_per_pixel;
        let scanline_pad = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == depth)
            .map_or(32, |format| usize::from(format.scanline_pad));
        let padded = stride.div_ceil(scanline_pad / 8) * (scanline_pad / 8);
        let mut rgb = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
        for row in 0..usize::from(height) {
            let start = row * padded;
            for column in 0..usize::from(width) {
                let offset = start + column * bytes_per_pixel;
                let Some(chunk) = data.get(offset..offset + bytes_per_pixel) else {
                    return Err(X11Error::AgentInput(
                        "the server returned a short image".to_owned(),
                    ));
                };
                let mut pixel = 0_u32;
                for (index, byte) in chunk.iter().enumerate() {
                    pixel |= u32::from(*byte) << (8 * index);
                }
                rgb.push(channel(pixel, red_mask));
                rgb.push(channel(pixel, green_mask));
                rgb.push(channel(pixel, blue_mask));
            }
        }
        if let Some((grid, origin)) = grid {
            render_capture_grid(
                &mut rgb,
                usize::from(width),
                usize::from(height),
                grid.spacing,
                origin,
            );
        }
        let mut encoded = Vec::new();
        let mut encoder = png::Encoder::new(&mut encoded, u32::from(width), u32::from(height));
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&rgb))
            .map_err(|error| {
                X11Error::AgentInput(format!("could not encode the capture: {error}"))
            })?;
        Ok(encoded)
    }

    /// Ends a session after a fault it cannot recover from.
    fn agent_fault(&mut self, session: AgentSessionId, error: &AgentError) {
        warn!(session = %session, %error, "ending an agent session");
        if let Some(seat) = self.agent_seat.as_mut() {
            seat.close(
                session,
                nobox_agent_wire::DisconnectReason::ProtocolViolation,
                &error.message,
            );
        }
        self.close_agent_session(session);
    }

    fn close_agent_session(&mut self, session: AgentSessionId) {
        if self
            .agent_text
            .as_ref()
            .is_some_and(|pending| pending.session == session)
            && let Some(pending) = self.agent_text.take()
        {
            let _ = self.runtime_timer.cancel_agent_text(pending.generation);
        }
        let generations: Vec<u32> = self
            .agent_observations
            .iter()
            .filter_map(|(generation, pending)| (pending.session == session).then_some(*generation))
            .collect();
        for generation in generations {
            self.agent_observations.remove(&generation);
            let _ = self.runtime_timer.cancel_agent_observation(generation);
        }
        let semantic_generations = self
            .agent_semantics
            .iter()
            .filter_map(|(generation, pending)| (pending.session == session).then_some(*generation))
            .collect::<Vec<_>>();
        for generation in semantic_generations {
            self.agent_semantics.remove(&generation);
            let _ = self.runtime_timer.cancel_agent_semantic(generation);
            if let Some(runner) = self.semantic_runner.as_ref() {
                runner.cancel(generation);
            }
        }
        self.agent_state.close(session);
        self.agent_semantic_trees
            .retain(|(tree_session, _), _| *tree_session != session);
        self.agent_scopes.remove(&session);
        self.agent_consented.remove(&session);
        if let Err(error) = self.refresh_agent_indicator() {
            warn!(%error, "could not update the agent seat indicator");
        }
    }

    /// Records a client's agent visibility and its scope membership.
    ///
    /// Called whenever a client is managed or its identity changes, so a rule
    /// that hides an application takes effect before any session can observe
    /// it.
    fn register_agent_client(&mut self, client: ClientId) {
        if self.agent_seat.is_none() {
            return;
        }
        let Some(identity) = self.application_identities.get(&client) else {
            return;
        };
        let identity = identity.as_application_identity();
        let visibility = match self
            .config
            .application_settings(identity)
            .agent_visibility
            .unwrap_or_default()
        {
            nobox_config::AgentVisibility::Visible => AgentClientVisibility::Visible,
            nobox_config::AgentVisibility::Redacted => AgentClientVisibility::Redacted,
            nobox_config::AgentVisibility::Hidden => AgentClientVisibility::Hidden,
        };
        let scopes = &self.agent_scopes;
        self.agent_state
            .observe_client(client, visibility, |session| {
                scopes
                    .get(&session)
                    .is_none_or(|matcher| matcher.matches(identity))
            });
    }

    /// Forgets a client that is no longer managed.
    fn forget_agent_client(&mut self, client: ClientId) {
        self.agent_semantic_trees
            .retain(|(_, tree_client), _| *tree_client != client);
        self.agent_state.forget_client(client);
    }

    /// Records whether the server can composite, which decides whether
    /// covered windows are capturable at all.
    fn query_composite(&mut self) {
        self.composite_version = match self.connection.composite_query_version(0, 4) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => Some((reply.major_version, reply.minor_version)),
                Err(_) => None,
            },
            Err(_) => None,
        };
        match self.composite_version {
            Some((major, minor)) => {
                info!(
                    major,
                    minor, "composite is available for covered-window capture"
                );
            }
            None => info!("composite is unavailable; covered windows are not capturable"),
        }
    }

    /// Asks the server for device-level input notifications.
    ///
    /// A window manager sees almost none of the user's input through ordinary
    /// events: keys go to the focused client, clicks go to the client under
    /// the pointer. "Human input preempts agent input" would be a promise the
    /// manager could not keep without this, so raw XInput2 events are selected
    /// on the root when an agent seat exists.
    fn select_raw_input(&mut self) -> Result<(), X11Error> {
        if self.agent_seat.is_none() || self.raw_input_selected {
            return Ok(());
        }
        let version = match self.connection.xinput_xi_query_version(2, 0) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => reply,
                Err(error) => {
                    warn!(%error, "XInput2 is unavailable; human input can only be seen where the manager already receives it");
                    return Ok(());
                }
            },
            Err(error) => {
                warn!(%error, "XInput2 is unavailable; human input can only be seen where the manager already receives it");
                return Ok(());
            }
        };
        if version.major_version < 2 {
            warn!(
                major = version.major_version,
                "XInput2 is too old for device-level input notifications"
            );
            return Ok(());
        }
        let mask = xinput::EventMask {
            // Master devices are what a human actually uses.
            deviceid: xinput::Device::ALL_MASTER.into(),
            mask: vec![
                xinput::XIEventMask::RAW_KEY_PRESS
                    | xinput::XIEventMask::RAW_KEY_RELEASE
                    | xinput::XIEventMask::RAW_BUTTON_PRESS
                    | xinput::XIEventMask::RAW_BUTTON_RELEASE
                    | xinput::XIEventMask::RAW_MOTION,
            ],
        };
        let selected = match self.connection.xinput_xi_select_events(self.root, &[mask]) {
            Ok(cookie) => cookie.check().map_err(X11Error::from),
            Err(error) => Err(X11Error::from(error)),
        };
        if let Err(error) = selected {
            warn!(%error, "could not select device-level input notifications");
            return Ok(());
        }
        self.raw_input_selected = true;
        info!("watching device-level input so the human always preempts the agent seat");
        Ok(())
    }

    /// Starts the agent seat when configuration asks for one, and advertises
    /// it on the root window the traditional way, so a companion discovers the
    /// protocol version and socket path without a side channel.
    ///
    /// A seat that cannot start is logged and skipped: window management never
    /// depends on it.
    fn start_agent_seat(&mut self) -> Result<(), X11Error> {
        if !self.config.agent.enabled
            || self.agent_seat.is_some()
            || self.agent_seat_ownership.is_some()
        {
            return Ok(());
        }
        let display = self.agent_display.clone();
        let control = match ControlSender::connect(
            display.as_deref(),
            self.support_window,
            self.atoms._NOBOX_CONTROL,
        ) {
            Ok(control) => control,
            Err(error) => {
                warn!(%error, "agent seat control channel is unavailable");
                return Ok(());
            }
        };
        let Some((owner_window, timestamp)) = self.claim_agent_seat_owner()? else {
            return Ok(());
        };
        let Some(mut seat) =
            agent::AgentSeat::prepare(&self.config.agent, display.as_deref(), control)
        else {
            self.release_agent_seat_owner(owner_window, &[]);
            return Ok(());
        };
        let advertisement = seat.advertisement().encode().into_bytes();
        if advertisement.len() > MAX_AGENT_ADVERTISEMENT_BYTES {
            warn!(
                bytes = advertisement.len(),
                limit = MAX_AGENT_ADVERTISEMENT_BYTES,
                "agent seat advertisement is too large"
            );
            self.release_agent_seat_owner(owner_window, &advertisement);
            return Ok(());
        }
        let ownership = AgentSeatOwnership {
            window: owner_window,
            timestamp,
            advertisement,
        };
        match self.publish_agent_seat(&ownership) {
            Ok(true) => {}
            Ok(false) => {
                self.release_agent_seat_owner(ownership.window, &ownership.advertisement);
                return Ok(());
            }
            Err(error) => {
                self.release_agent_seat_owner(ownership.window, &ownership.advertisement);
                return Err(error);
            }
        }
        if let Err(error) = seat.activate() {
            warn!(%error, "could not activate the agent seat listener");
            seat.stop();
            self.release_agent_seat_owner(ownership.window, &ownership.advertisement);
            return Ok(());
        }
        self.agent_seat_ownership = Some(ownership);
        self.agent_seat = Some(seat);
        self.select_raw_input()?;
        self.query_composite();
        Ok(())
    }

    fn claim_agent_seat_owner(&mut self) -> Result<Option<(Window, u32)>, X11Error> {
        let owner = self
            .connection
            .get_selection_owner(self.agent_selection)?
            .reply()?
            .owner;
        if owner != NONE {
            warn!(
                owner = format_args!("{owner:#x}"),
                screen = self.screen_index,
                "agent seat not started because another provider owns the screen"
            );
            return Ok(None);
        }

        let window = self.connection.generate_id()?;
        self.connection
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                -1,
                -1,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )?
            .check()?;
        let timestamp = match server_timestamp(
            &self.connection,
            window,
            self.atoms._NOBOX_TIMESTAMP,
            &mut self.deferred_events,
        ) {
            Ok(timestamp) => timestamp,
            Err(error) => {
                let _ = self.connection.destroy_window(window);
                return Err(error);
            }
        };
        let claimed = self.with_server_grab(|| {
            let owner = self
                .connection
                .get_selection_owner(self.agent_selection)?
                .reply()?
                .owner;
            if owner != NONE {
                warn!(
                    owner = format_args!("{owner:#x}"),
                    screen = self.screen_index,
                    "agent seat ownership changed while Nobox was preparing"
                );
                return Ok(false);
            }
            self.connection
                .set_selection_owner(window, self.agent_selection, timestamp)?
                .check()?;
            let owner = self
                .connection
                .get_selection_owner(self.agent_selection)?
                .reply()?
                .owner;
            if owner != window {
                warn!(
                    screen = self.screen_index,
                    "could not claim the agent seat selection"
                );
                return Ok(false);
            }
            Ok(true)
        });
        match claimed {
            Ok(true) => Ok(Some((window, timestamp))),
            Ok(false) => {
                let _ = self.connection.destroy_window(window);
                Ok(None)
            }
            Err(error) => {
                let _ = self.connection.destroy_window(window);
                Err(error)
            }
        }
    }

    fn publish_agent_seat(&self, ownership: &AgentSeatOwnership) -> Result<bool, X11Error> {
        let published = self.with_server_grab(|| {
            let owner = self
                .connection
                .get_selection_owner(self.agent_selection)?
                .reply()?
                .owner;
            if owner != ownership.window {
                warn!(
                    owner = format_args!("{owner:#x}"),
                    screen = self.screen_index,
                    "lost the agent seat selection before publication"
                );
                return Ok(false);
            }
            for window in [ownership.window, self.root] {
                self.connection
                    .change_property8(
                        x11rb::protocol::xproto::PropMode::REPLACE,
                        window,
                        self.atoms._AGENT_SEAT,
                        self.atoms.UTF8_STRING,
                        &ownership.advertisement,
                    )?
                    .check()?;
            }
            let announcement = ClientMessageEvent::new(
                32,
                self.root,
                self.atoms.MANAGER,
                [
                    ownership.timestamp,
                    self.agent_selection,
                    ownership.window,
                    0,
                    0,
                ],
            );
            self.connection
                .send_event(false, self.root, EventMask::STRUCTURE_NOTIFY, announcement)?
                .check()?;
            Ok(true)
        })?;
        self.connection.flush()?;
        Ok(published)
    }

    fn with_server_grab<T>(
        &self,
        operation: impl FnOnce() -> Result<T, X11Error>,
    ) -> Result<T, X11Error> {
        self.connection.grab_server()?.check()?;
        let result = operation();
        let ungrabbed = self.connection.ungrab_server()?.check();
        if let Err(error) = ungrabbed {
            return Err(error.into());
        }
        result
    }

    /// Withdraws the advertisement and ends every agent session.
    fn stop_agent_seat(&mut self) {
        if let Some(mut seat) = self.agent_seat.take() {
            seat.stop();
        }
        // The sessions went with the socket; nothing may outlive it.
        for session in self
            .agent_state
            .sessions()
            .map(|(session, _)| session)
            .collect::<Vec<_>>()
        {
            self.agent_state.close(session);
        }
        self.agent_scopes.clear();
        self.agent_consented.clear();
        self.agent_shadow.clear();
        if let Some(ownership) = self.agent_seat_ownership.take() {
            self.release_agent_seat_owner(ownership.window, &ownership.advertisement);
        }
    }

    fn release_agent_seat_owner(&self, window: Window, advertisement: &[u8]) {
        let released = self.with_server_grab(|| {
            let owner = self
                .connection
                .get_selection_owner(self.agent_selection)?
                .reply()?
                .owner;
            let root_is_ours = owner == window
                && !advertisement.is_empty()
                && self
                    .agent_advertisement(self.root)?
                    .is_some_and(|value| value == advertisement);
            if root_is_ours {
                self.connection
                    .delete_property(self.root, self.atoms._AGENT_SEAT)?
                    .check()?;
            }
            self.connection.destroy_window(window)?.check()?;
            Ok(())
        });
        if let Err(error) = released {
            warn!(%error, "could not release the agent seat owner window");
        }
    }

    fn agent_advertisement(&self, window: Window) -> Result<Option<Vec<u8>>, X11Error> {
        let reply = self
            .connection
            .get_property(
                false,
                window,
                self.atoms._AGENT_SEAT,
                AtomEnum::ANY,
                0,
                MAX_AGENT_ADVERTISEMENT_LONGS,
            )?
            .reply()?;
        if reply.type_ == NONE {
            return Ok(None);
        }
        if reply.type_ != self.atoms.UTF8_STRING
            || reply.format != 8
            || reply.bytes_after != 0
            || reply.value.len() > MAX_AGENT_ADVERTISEMENT_BYTES
        {
            return Ok(None);
        }
        Ok(Some(reply.value))
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
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.support_window,
            self.atoms._NET_WM_PID,
            AtomEnum::CARDINAL,
            &[std::process::id()],
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
            self.atoms._NET_WM_FULL_PLACEMENT,
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
            self.atoms._NET_WM_PID,
            self.atoms._NET_WM_WINDOW_OPACITY,
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
            self.atoms._NET_STARTUP_ID,
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
        let effective_bindings = self.config.effective_key_bindings();
        let mut key_bindings = KeyBindingNode::default();
        for binding in &effective_bindings {
            let sequence = binding
                .key
                .chords()
                .iter()
                .map(&resolve_chord)
                .collect::<Result<Vec<_>, _>>()?;
            insert_key_binding_variants(&mut key_bindings, &sequence, &binding.actions)?;
        }
        self.chain_quit_bindings = resolve_chord(&self.config.keyboard.chain_quit_key)?;
        self.agent_kill_chord = if self.config.agent.enabled {
            resolve_chord(&self.config.agent.kill_chord)?
        } else {
            Vec::new()
        };
        self.keyboard_layout = Some(KeyboardLayout {
            minimum,
            per_keycode: mapping.keysyms_per_keycode,
            keysyms: mapping.keysyms.clone(),
        });
        self.key_bindings = key_bindings;
        self.grab_current_key_bindings()?;
        self.reload_mouse_bindings()?;
        info!(
            bindings = effective_bindings.len(),
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

    /// Serves one bounded UTF-8 selection offer only to the target X11 client.
    fn agent_text_selection_request(
        &mut self,
        event: &SelectionRequestEvent,
    ) -> Result<(), X11Error> {
        let Some(pending) = self.agent_text.as_ref() else {
            return Ok(());
        };
        let PendingAgentTextPlan::TransferOffered {
            text, client_base, ..
        } = &pending.plan
        else {
            return Ok(());
        };
        if event.owner != self.support_window || event.selection != self.atoms.CLIPBOARD {
            return Ok(());
        }

        let terminal_error = self.pending_agent_text_error(pending);
        let current_owner = self
            .connection
            .get_selection_owner(self.atoms.CLIPBOARD)?
            .reply()?
            .owner;
        let same_target_client = xres_client_base(&self.connection, window_id(pending.target))
            .is_some_and(|current| current == *client_base);
        let same_requestor_client = xres_client_base(&self.connection, event.requestor)
            .is_some_and(|requestor| requestor == *client_base);
        let admitted = terminal_error.is_none()
            && current_owner == self.support_window
            && same_target_client
            && same_requestor_client;
        let property = if event.property == NONE {
            event.target
        } else {
            event.property
        };
        let mut delivered_text = false;
        let converted = if admitted && property != NONE && event.target == self.atoms.TARGETS {
            self.connection
                .change_property32(
                    x11rb::protocol::xproto::PropMode::REPLACE,
                    event.requestor,
                    property,
                    AtomEnum::ATOM,
                    &[
                        self.atoms.TARGETS,
                        self.atoms.UTF8_STRING,
                        self.atoms.TEXT_PLAIN_UTF8,
                        self.atoms.TEXT_PLAIN,
                    ],
                )?
                .check()?;
            true
        } else if admitted
            && property != NONE
            && matches!(
                event.target,
                target if target == self.atoms.UTF8_STRING
                    || target == self.atoms.TEXT_PLAIN_UTF8
                    || target == self.atoms.TEXT_PLAIN
            )
        {
            self.connection
                .change_property8(
                    x11rb::protocol::xproto::PropMode::REPLACE,
                    event.requestor,
                    property,
                    event.target,
                    text.as_bytes(),
                )?
                .check()?;
            delivered_text = true;
            true
        } else {
            false
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
            .send_event(false, event.requestor, EventMask::NO_EVENT, notify)?
            .check()?;
        self.connection.flush()?;

        if let Some(error) = terminal_error {
            let pending = self
                .agent_text
                .take()
                .expect("the exact-text request was inspected above");
            let _ = self.runtime_timer.cancel_agent_text(pending.generation);
            self.finish_agent_text_error(pending, error);
        } else if delivered_text {
            let now = Instant::now();
            let (generation, finish_at) = {
                let pending = self
                    .agent_text
                    .as_mut()
                    .expect("the delivered exact-text request was inspected above");
                let PendingAgentTextPlan::TransferOffered {
                    deadline,
                    last_delivery,
                    ..
                } = &mut pending.plan
                else {
                    unreachable!("the exact-text offer was inspected above")
                };
                *last_delivery = Some(now);
                (
                    pending.generation,
                    agent_text_transfer_finish_at(*deadline, Some(now)),
                )
            };
            let _ = self.runtime_timer.cancel_agent_text(generation);
            if let Err(error) = self
                .runtime_timer
                .arm_agent_text(generation, finish_at.saturating_duration_since(now))
            {
                let pending = self
                    .agent_text
                    .take()
                    .expect("the exact-text request was just updated");
                let _ = self.release_agent_text_selection();
                self.finish_agent_text_error(
                    pending,
                    AgentError::new(AgentErrorCode::Internal, error.to_string()),
                );
            }
        }
        Ok(())
    }

    fn pending_agent_text_error(&self, pending: &PendingAgentText) -> Option<AgentError> {
        if let Err(error) = self.agent_state.authorize(pending.session, &pending.call) {
            return Some(error);
        }
        if !self.agent_state.perceives(pending.session, pending.target)
            || matches!(
                self.agent_state.visibility(pending.target),
                AgentClientVisibility::Redacted
            )
            || self.clients.get(pending.target).is_none()
        {
            return Some(AgentError::no_such_client());
        }
        if self.clients.focused() != Some(pending.target) {
            return Some(AgentError::stale_state(
                self.agent_state.generation(pending.target),
            ));
        }
        self.agent_input_suppressed()
            .then(|| AgentError::interrupted(pending.committed.clone()))
    }

    fn agent_text_selection_lost(&mut self) {
        if self.agent_text.as_ref().is_none_or(|pending| {
            !matches!(pending.plan, PendingAgentTextPlan::TransferOffered { .. })
        }) {
            return;
        }
        let pending = self
            .agent_text
            .take()
            .expect("the pending exact-text request was inspected above");
        let _ = self.runtime_timer.cancel_agent_text(pending.generation);
        let error = AgentError::interrupted(pending.committed.clone());
        self.finish_agent_text_error(pending, error);
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
        // The kill chord is grabbed in every chain state, so freezing agent
        // sessions never depends on what else the keyboard is in the middle of.
        for &(keycode, modifiers) in node
            .children
            .keys()
            .chain(
                self.key_chain
                    .iter()
                    .flat_map(|_| self.chain_quit_bindings.iter()),
            )
            .chain(self.agent_kill_chord.iter())
        {
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
        let mut bindings = BTreeMap::new();
        for modifier in self.config.mouse.effective_modifiers() {
            let modifiers = u16::from(Self::modifier_mask(modifier));
            for (button, action) in [
                (self.config.mouse.move_button, Action::Move),
                (
                    self.config.mouse.resize_button,
                    Action::Resize { edge: None },
                ),
            ] {
                bindings.insert(
                    MouseBindingKey {
                        context: MouseContext::Frame,
                        button,
                        modifiers,
                        trigger: MouseTrigger::Drag,
                    },
                    vec![action],
                );
            }
        }
        for binding in self.config.mouse.effective_bindings() {
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
        for id in self.clients.management_order() {
            self.grab_client_mouse_bindings(id)?;
        }
        info!(
            bindings = self.mouse_bindings.len(),
            "loaded X11 mouse bindings"
        );
        Ok(())
    }

    fn grab_client_mouse_bindings(&self, id: ClientId) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(id) else {
            return Ok(());
        };
        let window = window_id(id);
        if !window_request_succeeded(
            self.connection
                .ungrab_button(ButtonIndex::ANY, window, ModMask::ANY)?
                .check(),
        )? {
            return Ok(());
        }
        let context = if client.policy.role == ClientRole::Desktop {
            MouseContext::Desktop
        } else {
            MouseContext::Client
        };
        let mut buttons = self
            .mouse_bindings
            .keys()
            .filter(|binding| {
                binding.modifiers == 0 && mouse_context_chain(context).contains(&binding.context)
            })
            .map(|binding| binding.button)
            .collect::<BTreeSet<_>>();
        if context == MouseContext::Desktop {
            buttons.insert(u8::from(ButtonIndex::M1));
        }
        for button in buttons {
            for locks in lock_combinations(self.ignored_modifiers) {
                let result = self
                    .connection
                    .grab_button(
                        false,
                        window,
                        EventMask::BUTTON_PRESS,
                        GrabMode::SYNC,
                        GrabMode::ASYNC,
                        NONE,
                        NONE,
                        ButtonIndex::from(button),
                        ModMask::from(locks),
                    )?
                    .check();
                match result {
                    Ok(()) => {}
                    Err(ReplyError::X11Error(error))
                        if matches!(error.error_kind, ErrorKind::Access | ErrorKind::Window) =>
                    {
                        warn!(
                            window = format_args!("{window:#x}"),
                            button,
                            modifiers = locks,
                            ?error.error_kind,
                            "could not install client mouse grab"
                        );
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(())
    }

    fn modifier_mask(modifier: MouseModifier) -> ModMask {
        match modifier {
            MouseModifier::Alt => ModMask::M1,
            MouseModifier::Super => ModMask::M4,
        }
    }

    fn reload_config(&mut self, config: Config) -> Result<(), X11Error> {
        self.application_catalog = ApplicationCatalog::discover();
        if config == self.config {
            if config.agent.enabled
                && (self.agent_seat.is_none() || self.agent_seat_ownership.is_none())
            {
                self.start_agent_seat()?;
                return Ok(());
            }
            info!("configuration reload contained no changes");
            return Ok(());
        }
        if config.agent.grants != self.config.agent.grants {
            self.reapply_agent_grants(&config);
        }
        let agent_seat_changed = config.agent.enabled != self.agent_seat.is_some()
            || config.agent.socket != self.config.agent.socket;
        self.cancel_drag(self.last_timestamp)?;
        self.hide_menu(self.last_timestamp)?;
        let colormap = self.connection.setup().roots[self.screen_index].default_colormap;
        let new_pixels = DecorationPixels::allocate(&self.connection, colormap, &config.theme)?;
        let new_font = if config.theme.font == self.config.theme.font {
            None
        } else {
            match load_title_font(&self.connection, &config.theme.font) {
                Ok(font) => Some(font),
                Err(error) => {
                    self.connection
                        .free_colors(colormap, 0, &new_pixels.as_array())?;
                    return Err(error);
                }
            }
        };
        let previous_config = std::mem::replace(&mut self.config, config);
        if let Err(error) = self.reload_input_bindings() {
            self.config = previous_config;
            self.reload_input_bindings()?;
            if let Some(font) = &new_font {
                let _ = self.connection.close_font(font.id);
            }
            self.connection
                .free_colors(colormap, 0, &new_pixels.as_array())?;
            return Err(error);
        }

        if agent_seat_changed {
            // Turning the seat on or off, or moving its socket, takes effect
            // now: a setting that needed a restart would be a setting people
            // could not trust in a hurry.
            self.stop_agent_seat();
            if let Err(error) = self.start_agent_seat() {
                warn!(%error, "could not start the agent seat after a configuration reload");
            }
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
        if previous_config.margins != self.config.margins {
            let _ = self.refresh_work_area()?;
        }

        let previous_pixels = std::mem::replace(&mut self.decoration_pixels, new_pixels);
        let mut title_gc = ChangeGCAux::new().foreground(self.decoration_pixels.title_text);
        if let Some(font) = &new_font {
            title_gc = title_gc.font(font.id);
        }
        self.connection.change_gc(self.title_gc, &title_gc)?;
        let previous_font = new_font.map(|font| std::mem::replace(&mut self.title_font, font));
        for window in self.focus_indicator.windows {
            self.connection.change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new()
                    .background_pixel(self.decoration_pixels.active_border),
            )?;
        }
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
        if let Some(font) = previous_font {
            self.connection.close_font(font.id)?;
        }
        info!("reloaded configuration in place");
        Ok(())
    }

    fn refresh_frame_colors(&self, id: ClientId) -> Result<(), X11Error> {
        let Some(frame) = self.frames.get(&id).copied() else {
            return Ok(());
        };
        let Some(client) = self.clients.get(id) else {
            return Ok(());
        };
        let (border, titlebar) = if self
            .agent_input_target
            .is_some_and(|(target, _)| target == id)
        {
            // A window receiving agent input is marked by the manager itself.
            // The protocol offers no way to draw, cover, or dismiss this.
            (
                self.decoration_pixels.agent_marker,
                self.decoration_pixels.active_titlebar,
            )
        } else if self.unresponsive_clients.contains(&id) {
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
        if frame.extents == DecorationExtents::default() {
            self.connection.change_window_attributes(
                frame.window,
                &ChangeWindowAttributesAux::new().background_pixmap(BackPixmap::PARENT_RELATIVE),
            )?;
        } else {
            self.connection.change_window_attributes(
                frame.window,
                &ChangeWindowAttributesAux::new().background_pixel(titlebar),
            )?;
        }
        self.connection
            .clear_area(false, frame.window, 0, 0, 0, 0)?;
        let outer_width = client
            .geometry
            .width
            .saturating_add(frame.extents.left)
            .saturating_add(frame.extents.right);
        let outer_height = if client.shaded {
            frame.extents.top.saturating_add(frame.extents.bottom)
        } else {
            client
                .geometry
                .height
                .saturating_add(frame.extents.top)
                .saturating_add(frame.extents.bottom)
        };
        let side_height = outer_height
            .saturating_sub(frame.extents.left)
            .saturating_sub(frame.extents.bottom);
        self.connection
            .change_gc(self.title_gc, &ChangeGCAux::new().foreground(border))?;
        self.connection.poly_fill_rectangle(
            frame.window,
            self.title_gc,
            &[
                Rectangle {
                    x: 0,
                    y: 0,
                    width: x_dimension(outer_width),
                    height: x_dimension(frame.extents.left),
                },
                Rectangle {
                    x: 0,
                    y: clamp_i16_u32(outer_height.saturating_sub(frame.extents.bottom)),
                    width: x_dimension(outer_width),
                    height: x_dimension(frame.extents.bottom),
                },
                Rectangle {
                    x: 0,
                    y: clamp_i16_u32(frame.extents.left),
                    width: x_dimension(frame.extents.left),
                    height: x_dimension(
                        outer_height
                            .saturating_sub(frame.extents.left)
                            .saturating_sub(frame.extents.bottom),
                    ),
                },
                Rectangle {
                    x: clamp_i16_u32(outer_width.saturating_sub(frame.extents.right)),
                    y: clamp_i16_u32(frame.extents.left),
                    width: x_dimension(frame.extents.right),
                    height: x_dimension(side_height),
                },
            ],
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
                self.draw_frame_button(button)?;
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

    fn refresh_client_opacity(&self, window: Window) -> Result<(), X11Error> {
        let Some(frame) = self.frames.get(&client_id(window)) else {
            return Ok(());
        };
        let opacity = self
            .connection
            .get_property(
                false,
                window,
                self.atoms._NET_WM_WINDOW_OPACITY,
                AtomEnum::CARDINAL,
                0,
                1,
            )?
            .reply()?
            .value32()
            .and_then(|mut values| values.next());
        if let Some(opacity) = opacity {
            self.connection.change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                frame.window,
                self.atoms._NET_WM_WINDOW_OPACITY,
                AtomEnum::CARDINAL,
                &[opacity],
            )?;
        } else {
            self.connection
                .delete_property(frame.window, self.atoms._NET_WM_WINDOW_OPACITY)?;
        }
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

    fn refresh_user_time_window(&mut self, window: Window) -> Result<(), X11Error> {
        let time_window = self.read_window_property(window, self.atoms._NET_WM_USER_TIME_WINDOW)?;
        self.refresh_user_time_window_from(window, time_window)
    }

    fn refresh_user_time_window_from(
        &mut self,
        window: Window,
        time_window: Option<Window>,
    ) -> Result<(), X11Error> {
        let id = client_id(window);
        if let Some(previous) = self.client_user_time_windows.remove(&id)
            && self.user_time_windows.get(&previous) == Some(&id)
        {
            self.user_time_windows.remove(&previous);
        }
        let Some(time_window) = time_window else {
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
        let focused_colormaps = self
            .clients
            .focused()
            .and_then(|focused| self.client_colormaps.get(&focused));
        // Fast path for the overwhelmingly common case: the focused client has
        // no colormap windows and only the default colormap is installed.
        if focused_colormaps.is_none_or(|colormaps| colormaps.is_empty())
            && self.active_colormaps == [self.default_colormap]
        {
            return Ok(());
        }
        let mut desired = Vec::new();
        let mut seen = BTreeSet::new();
        if let Some(colormaps) = focused_colormaps {
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
        if desired == self.active_colormaps {
            return Ok(());
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
            group_name: String::new(),
            group_class: String::new(),
            role: x11_text(&role_reply.value),
            title: self.read_title(window)?,
            kind: application_kind(role),
        };
        let group = self
            .clients
            .get(client_id(window))
            .and_then(|client| client.group)
            .map(window_id);
        let (group_name, group_class) = self.read_group_class(group)?;
        let identity = X11ApplicationIdentity {
            group_name,
            group_class,
            ..identity
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

    fn read_group_class(&self, group: Option<Window>) -> Result<(String, String), X11Error> {
        let Some(group) = group else {
            return Ok((String::new(), String::new()));
        };
        match self
            .connection
            .get_property(false, group, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 2048)?
            .reply()
        {
            Ok(reply) => Ok(parse_wm_class(&reply.value)),
            Err(error) => {
                let error = X11Error::from(error);
                if error.is_vanished_window() {
                    Ok((String::new(), String::new()))
                } else {
                    Err(error)
                }
            }
        }
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

    fn read_startup_id(&self, window: Window) -> Result<Option<String>, X11Error> {
        let reply = self
            .connection
            .get_property(
                false,
                window,
                self.atoms._NET_STARTUP_ID,
                self.atoms.UTF8_STRING,
                0,
                1024,
            )?
            .reply()?;
        if reply.bytes_after != 0 || reply.value.is_empty() || reply.value.contains(&0) {
            return Ok(None);
        }
        Ok(String::from_utf8(reply.value).ok())
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

    fn refresh_application_settings(
        &mut self,
        window: Window,
        role: ClientRole,
    ) -> Result<ApplicationSettings, X11Error> {
        let identity = self.read_application_identity(window, role)?;
        let settings = self
            .config
            .application_settings(identity.as_application_identity());
        self.application_identities
            .insert(client_id(window), identity);
        Ok(settings)
    }

    fn refresh_title(&mut self, window: Window) -> Result<(), X11Error> {
        let title = self.read_title(window)?;
        self.apply_title(window, title)
    }

    fn apply_title(&mut self, window: Window, title: String) -> Result<(), X11Error> {
        let id = client_id(window);
        // Many applications re-announce an unchanged title; the stored copy is
        // updated together with the identity, so an equal title needs no work.
        if self.titles.get(&id) == Some(&title) {
            return Ok(());
        }
        if let Some(identity) = self.application_identities.get_mut(&id) {
            identity.title.clone_from(&title);
        }
        let frame = self.frames.get(&id).copied();
        let Some(frame) = frame else {
            self.titles.insert(id, title);
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
        self.titles.insert(id, title);
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
            clamp_i16_u32(frame.extents.left),
            clamp_i16_u32(frame.extents.left),
            x_dimension(client.geometry.width),
            x_dimension(titlebar_height),
        )?;
        let button_count = u32::from(frame.minimize_button.is_some())
            .saturating_add(u32::from(frame.maximize_button.is_some()))
            .saturating_add(u32::from(frame.close_button.is_some()));
        let button_size = titlebar_height.saturating_sub(8).max(1);
        let button_area = button_count.saturating_mul(button_size.saturating_add(4));
        let text_left = frame
            .extents
            .left
            .saturating_add(self.config.theme.title_padding.min(client.geometry.width));
        let text_right = client
            .geometry
            .width
            .saturating_sub(button_area)
            .saturating_sub(self.config.theme.title_padding)
            .saturating_add(frame.extents.left)
            .max(text_left);
        let title = self.titles.get(&id).map_or("", String::as_str);
        let title = if unresponsive {
            Cow::Owned(format!("{title} (Not Responding)"))
        } else {
            Cow::Borrowed(title)
        };
        let (text, text_width) = fitted_title_text(
            &title,
            text_right.saturating_sub(text_left),
            255,
            &self.title_font.metrics,
        );
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
                aligned_text_x(
                    self.config.theme.title_alignment,
                    text_left,
                    text_right,
                    text_width,
                ),
                text_baseline(
                    frame.extents.left,
                    titlebar_height,
                    &self.title_font.metrics,
                ),
                &text,
            )?;
        }
        for button in [
            frame.minimize_button,
            frame.maximize_button,
            frame.close_button,
        ]
        .into_iter()
        .flatten()
        {
            self.draw_frame_button(button)?;
        }
        Ok(())
    }

    fn draw_frame_button(&self, window: Window) -> Result<(), X11Error> {
        let Some(FramePart::Button(id, kind)) = self.frame_parts.get(&window).copied() else {
            return Ok(());
        };
        let Some(frame) = self.frames.get(&id).copied() else {
            return Ok(());
        };
        let Some(client) = self.clients.get(id) else {
            return Ok(());
        };
        let size = frame
            .extents
            .top
            .saturating_sub(frame.extents.left)
            .saturating_sub(8)
            .max(1)
            .min(client.geometry.width);
        self.connection.clear_area(false, window, 0, 0, 0, 0)?;
        let hovered = self.hovered_frame_button == Some(window);
        let pressed = hovered && self.pressed_frame_button == Some(window);
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new()
                .foreground(self.decoration_pixels.button_glyph)
                .line_width(if pressed { 2_u32 } else { 1_u32 }),
        )?;
        if hovered && size > 4 {
            let edge = size.saturating_sub(2);
            self.connection.poly_fill_rectangle(
                window,
                self.title_gc,
                &[
                    Rectangle {
                        x: 1,
                        y: 1,
                        width: x_dimension(edge),
                        height: 1,
                    },
                    Rectangle {
                        x: 1,
                        y: clamp_i16_u32(size.saturating_sub(2)),
                        width: x_dimension(edge),
                        height: 1,
                    },
                    Rectangle {
                        x: 1,
                        y: 1,
                        width: 1,
                        height: x_dimension(edge),
                    },
                    Rectangle {
                        x: clamp_i16_u32(size.saturating_sub(2)),
                        y: 1,
                        width: 1,
                        height: x_dimension(edge),
                    },
                ],
            )?;
        }
        let maximized = client
            .maximize
            .is_some_and(|state| state.horizontal && state.vertical);
        let (segments, segment_count) =
            frame_button_segments(kind, size, maximized, u32::from(pressed));
        self.connection
            .poly_segment(window, self.title_gc, &segments[..segment_count])?;
        Ok(())
    }

    fn set_hovered_frame_button(&mut self, window: Option<Window>) -> Result<(), X11Error> {
        let window = window
            .filter(|window| matches!(self.frame_parts.get(window), Some(FramePart::Button(_, _))));
        if window == self.hovered_frame_button {
            return Ok(());
        }
        let previous = std::mem::replace(&mut self.hovered_frame_button, window);
        debug!(button = ?window, "updated frame-button hover state");
        if let Some(previous) = previous {
            self.draw_frame_button(previous)?;
        }
        if let Some(window) = window {
            self.draw_frame_button(window)?;
        }
        Ok(())
    }

    fn set_pressed_frame_button(&mut self, window: Option<Window>) -> Result<(), X11Error> {
        let window = window
            .filter(|window| matches!(self.frame_parts.get(window), Some(FramePart::Button(_, _))));
        if window == self.pressed_frame_button {
            return Ok(());
        }
        let previous = std::mem::replace(&mut self.pressed_frame_button, window);
        debug!(button = ?window, "updated frame-button pressed state");
        if let Some(previous) = previous {
            self.draw_frame_button(previous)?;
        }
        if let Some(window) = window {
            self.draw_frame_button(window)?;
        }
        Ok(())
    }

    fn forget_frame_button(&mut self, window: Window) {
        if self.hovered_frame_button == Some(window) {
            self.hovered_frame_button = None;
        }
        if self.pressed_frame_button == Some(window) {
            self.pressed_frame_button = None;
        }
        self.frame_parts.remove(&window);
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
        extents: DecorationExtents,
        kind: FrameButtonKind,
        slot: u32,
    ) -> Result<Window, X11Error> {
        let button = self.connection.generate_id()?;
        let titlebar_height = extents.top.saturating_sub(extents.left);
        let border_width = extents.left;
        let size = titlebar_height.saturating_sub(8).max(1).min(content_width);
        let x = border_width.saturating_add(
            content_width.saturating_sub(
                size.saturating_add(4)
                    .saturating_mul(slot.saturating_add(1)),
            ),
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
            clamp_i16_u32(border_width.saturating_add(4)),
            x_dimension(size),
            x_dimension(size),
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(pixel)
                .cursor(self.cursors.pointer)
                .event_mask(
                    EventMask::BUTTON_PRESS
                        | EventMask::BUTTON_RELEASE
                        | EventMask::BUTTON_MOTION
                        | EventMask::ENTER_WINDOW
                        | EventMask::LEAVE_WINDOW
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

    fn create_resize_handles(
        &mut self,
        id: ClientId,
        frame: Window,
        width: u32,
        height: u32,
        extents: DecorationExtents,
    ) -> Result<ResizeHandles, X11Error> {
        let mut handles = Vec::with_capacity(ResizeHandlePart::ALL.len());
        for part in ResizeHandlePart::ALL {
            let context = part.context();
            let geometry = resize_handle_geometry(part, width, height, extents);
            let window = self.connection.generate_id()?;
            self.connection.create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                frame,
                clamp_i16(geometry.x),
                clamp_i16(geometry.y),
                x_dimension(geometry.width),
                x_dimension(geometry.height),
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new()
                    .cursor(self.cursors.for_context(context))
                    .event_mask(
                        EventMask::BUTTON_PRESS
                            | EventMask::BUTTON_RELEASE
                            | EventMask::BUTTON_MOTION
                            | EventMask::ENTER_WINDOW,
                    ),
            )?;
            self.frame_parts
                .insert(window, FramePart::ResizeHandle(id, part));
            handles.push(ResizeHandle { window, part });
        }
        let handles: [ResizeHandle; 12] = handles
            .try_into()
            .expect("the fixed resize part list creates twelve handles");
        Ok(ResizeHandles(handles))
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
        let titlebar_height = if policy.decorations.titlebar {
            self.config.theme.titlebar_height
        } else {
            0
        };
        let frame_width = outer.width;
        let frame_height = outer.height;
        let frame_attributes = if extents == DecorationExtents::default() {
            CreateWindowAux::new().background_pixmap(BackPixmap::PARENT_RELATIVE)
        } else {
            CreateWindowAux::new().background_pixel(self.decoration_pixels.inactive_titlebar)
        }
        .cursor(self.cursors.pointer)
        .event_mask(
            EventMask::SUBSTRUCTURE_REDIRECT
                | EventMask::SUBSTRUCTURE_NOTIFY
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::BUTTON_MOTION
                | EventMask::ENTER_WINDOW
                | EventMask::FOCUS_CHANGE
                | EventMask::EXPOSURE,
        );
        self.connection.create_window(
            COPY_DEPTH_FROM_PARENT,
            frame,
            self.root,
            clamp_i16(outer.x),
            clamp_i16(outer.y),
            x_dimension(frame_width),
            x_dimension(frame_height),
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &frame_attributes,
        )?;

        let close_button = if titlebar_height == 0 || !policy.decorations.close {
            None
        } else {
            Some(self.create_frame_button(
                id,
                frame,
                content.width,
                extents,
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
                extents,
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
                extents,
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
        self.connection.reparent_window(
            client,
            frame,
            clamp_i16_u32(extents.left),
            clamp_i16_u32(extents.top),
        )?;
        self.connection
            .configure_window(client, &ConfigureWindowAux::new().border_width(0))?;
        let resize_handles =
            self.create_resize_handles(id, frame, frame_width, frame_height, extents)?;
        self.publish_frame_extents(client, extents)?;
        self.frame_parts.insert(frame, FramePart::Container(id));
        Ok(Frame {
            window: frame,
            minimize_button,
            maximize_button,
            close_button,
            resize_handles,
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
        if self.resize_handles_enabled(client_id(client)) {
            for handle in frame.resize_handles.iter() {
                self.connection.map_window(handle.window)?;
            }
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

    fn resize_handles_enabled(&self, id: ClientId) -> bool {
        self.clients.get(id).is_some_and(|client| {
            client.policy.decorations.border
                && self.config.theme.border_width > 0
                && client.operations().resizable
                && client.maximize.is_none()
                && !client.iconic
        })
    }

    fn sync_resize_handles(
        &self,
        id: ClientId,
        frame: Frame,
        width: u32,
        height: u32,
    ) -> Result<(), X11Error> {
        let enabled = self.resize_handles_enabled(id);
        for handle in frame.resize_handles.iter() {
            let geometry = resize_handle_geometry(handle.part, width, height, frame.extents);
            self.connection.configure_window(
                handle.window,
                &ConfigureWindowAux::new()
                    .x(geometry.x)
                    .y(geometry.y)
                    .width(geometry.width)
                    .height(geometry.height)
                    .stack_mode(StackMode::ABOVE),
            )?;
            if enabled {
                self.connection.map_window(handle.window)?;
            } else {
                self.connection.unmap_window(handle.window)?;
            }
        }
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
        let titlebar_height = frame.extents.top;
        let frame_width = client
            .geometry
            .width
            .saturating_add(frame.extents.left)
            .saturating_add(frame.extents.right);
        self.connection.shape_combine(
            SO::SET,
            kind,
            kind,
            frame.window,
            clamp_i16_u32(frame.extents.left),
            clamp_i16_u32(frame.extents.top),
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
                    width: x_dimension(frame_width),
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

    fn work_area_adjusted_origin(
        &self,
        requested: Geometry,
        policy: ClientPolicy,
        assignment: WorkspaceAssignment,
    ) -> Geometry {
        let workspace = match assignment {
            WorkspaceAssignment::Workspace(workspace) => workspace,
            WorkspaceAssignment::All => self.clients.current_workspace(),
        };
        let output = self.outputs.output_for(requested);
        let bounds = self
            .output_work_areas
            .get(&(output.id, workspace))
            .copied()
            .unwrap_or(output.geometry);
        positioned_origin_in_work_area(requested, bounds, self.decoration_extents(policy))
    }

    fn read_initial_client_metadata(
        &self,
        window: Window,
    ) -> Result<InitialClientMetadata, X11Error> {
        let geometry = self.connection.get_geometry(window)?;
        let wm_hints = WmHints::get(&self.connection, window)?;
        let normal_hints = WmSizeHints::get_normal_hints(&self.connection, window)?;
        let transient = self.connection.get_property(
            false,
            window,
            self.atoms.WM_TRANSIENT_FOR,
            AtomEnum::WINDOW,
            0,
            1,
        )?;
        let states = self.connection.get_property(
            false,
            window,
            self.atoms._NET_WM_STATE,
            AtomEnum::ATOM,
            0,
            u32::MAX,
        )?;
        let user_time = self.connection.get_property(
            false,
            window,
            self.atoms._NET_WM_USER_TIME,
            AtomEnum::CARDINAL,
            0,
            1,
        )?;
        let user_time_window = self.connection.get_property(
            false,
            window,
            self.atoms._NET_WM_USER_TIME_WINDOW,
            AtomEnum::WINDOW,
            0,
            1,
        )?;
        let window_types = self.connection.get_property(
            false,
            window,
            self.atoms._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            0,
            u32::MAX,
        )?;
        let motif = self.connection.get_property(
            false,
            window,
            self.atoms._MOTIF_WM_HINTS,
            self.atoms._MOTIF_WM_HINTS,
            0,
            5,
        )?;
        let class = self.connection.get_property(
            false,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            0,
            2048,
        )?;
        let role_property = self.connection.get_property(
            false,
            window,
            self.atoms.WM_WINDOW_ROLE,
            AtomEnum::STRING,
            0,
            1024,
        )?;
        let modern_title = self.connection.get_property(
            false,
            window,
            self.atoms._NET_WM_NAME,
            self.atoms.UTF8_STRING,
            0,
            1024,
        )?;
        let legacy_title = self.connection.get_property(
            false,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::ANY,
            0,
            1024,
        )?;
        let leader = self.connection.get_property(
            false,
            window,
            self.atoms.WM_CLIENT_LEADER,
            AtomEnum::WINDOW,
            0,
            1,
        )?;
        let session_id = self.connection.get_property(
            false,
            window,
            self.atoms.SM_CLIENT_ID,
            AtomEnum::STRING,
            0,
            256,
        )?;
        let command = self.connection.get_property(
            false,
            window,
            AtomEnum::WM_COMMAND,
            AtomEnum::STRING,
            0,
            1024,
        )?;
        let desktop = self.connection.get_property(
            false,
            window,
            self.atoms._NET_WM_DESKTOP,
            AtomEnum::CARDINAL,
            0,
            1,
        )?;
        let startup_id = self.connection.get_property(
            false,
            window,
            self.atoms._NET_STARTUP_ID,
            self.atoms.UTF8_STRING,
            0,
            1024,
        )?;

        let geometry = geometry.reply()?;
        let wm_hints = wm_hints.reply()?;
        let normal_hints = normal_hints.reply()?.unwrap_or_default();
        let transient = first_value32(&transient.reply()?);
        let states = values32(&states.reply()?);
        let user_time = first_value32(&user_time.reply()?);
        let user_time_window = first_value32(&user_time_window.reply()?).filter(|id| *id != NONE);
        let user_time = if user_time.is_some() {
            user_time
        } else if let Some(time_window) = user_time_window {
            match self.read_cardinal_property(time_window, self.atoms._NET_WM_USER_TIME) {
                Ok(timestamp) => timestamp,
                Err(error) if error.is_vanished_window() => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let transient_for = match transient {
            Some(parent) if parent == self.root => Some(TransientTarget::Group),
            Some(parent) if parent != window => Some(TransientTarget::Client(client_id(parent))),
            _ => None,
        };
        let relationships = Relationships {
            transient_for,
            group: wm_hints
                .as_ref()
                .and_then(|hints| hints.window_group)
                .map(client_id),
            modal: states.contains(&self.atoms._NET_WM_STATE_MODAL),
        };
        let role = values32(&window_types.reply()?)
            .into_iter()
            .find_map(|atom| self.client_role(atom))
            .unwrap_or(if transient_for.is_some() {
                ClientRole::Dialog
            } else {
                ClientRole::Normal
            });
        let motif = motif_hints_from_reply(&motif.reply()?);
        let policy = apply_motif_hints(ClientPolicy::for_role(role), motif);
        let class_reply = class.reply()?;
        let (name, class) = parse_wm_class(&class_reply.value);
        let role_reply = role_property.reply()?;
        let modern_title = modern_title.reply()?;
        let legacy_title = legacy_title.reply()?;
        let title = if modern_title.value.is_empty() {
            legacy_title.value.into_iter().map(char::from).collect()
        } else {
            String::from_utf8_lossy(&modern_title.value).into_owned()
        };
        let application = X11ApplicationIdentity {
            name,
            class,
            group_name: String::new(),
            group_class: String::new(),
            role: x11_text(&role_reply.value),
            title,
            kind: application_kind(role),
        };
        let (group_name, group_class) =
            self.read_group_class(relationships.group.map(window_id))?;
        let application = X11ApplicationIdentity {
            group_name,
            group_class,
            ..application
        };
        debug!(
            window = format_args!("{window:#x}"),
            name = application.name,
            class = application.class,
            role = application.role,
            title = application.title,
            kind = ?application.kind,
            "read X11 application identity"
        );
        let leader = first_value32(&leader.reply()?).filter(|id| *id != NONE);
        let session_id = bounded_text_from_reply(&session_id.reply()?);
        let command = command_from_reply(&command.reply()?);
        let desktop = first_value32(&desktop.reply()?);
        let mut startup_id = bounded_text_from_reply(&startup_id.reply()?);
        if startup_id.is_none()
            && let Some(leader) = leader.filter(|leader| *leader != window)
        {
            startup_id = self.read_startup_id(leader)?;
        }

        Ok(InitialClientMetadata {
            geometry: InitialGeometry {
                x: geometry.x,
                y: geometry.y,
                width: geometry.width,
                height: geometry.height,
                border_width: geometry.border_width,
            },
            initially_iconic: matches!(
                wm_hints.and_then(|hints| hints.initial_state),
                Some(WmHintsState::Iconic)
            ),
            urgent: wm_hints.is_some_and(|hints| hints.urgent),
            normal_hints: normal_hints_from_wm(normal_hints),
            relationships,
            user_time,
            user_time_window,
            states,
            policy,
            application,
            leader,
            session_id,
            command,
            desktop,
            startup_id,
        })
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

        let metadata = self.read_initial_client_metadata(window)?;
        let geometry = metadata.geometry;
        let mut initially_iconic = map && metadata.initially_iconic;
        let normal_hints = metadata.normal_hints;
        let size_hints = normal_hints.size;
        let relationships = metadata.relationships;
        let mut user_time = metadata.user_time;
        let initial_states = metadata.states;
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
            urgent: metadata.urgent
                || initial_states.contains(&self.atoms._NET_WM_STATE_DEMANDS_ATTENTION),
        };
        let client_layer = client_layer_from_states(
            &initial_states,
            self.atoms._NET_WM_STATE_ABOVE,
            self.atoms._NET_WM_STATE_BELOW,
        );
        let client_policy = metadata.policy;
        let application_identity = metadata.application;
        let startup_sequence =
            self.match_startup_sequence(metadata.startup_id.as_deref(), &application_identity)?;
        if let Some(timestamp) = startup_sequence
            .as_ref()
            .and_then(|(_, sequence)| sequence.timestamp)
            && user_time.is_none_or(|user_time| x11_time_after(timestamp, user_time))
        {
            user_time = Some(timestamp);
        }
        let initial_title = application_identity.title.clone();
        let application = self
            .config
            .application_settings(application_identity.as_application_identity());
        initially_iconic = application.minimized.unwrap_or(initially_iconic);
        initially_shaded = application.shaded.unwrap_or(initially_shaded);
        initially_fullscreen = application.fullscreen.unwrap_or(initially_fullscreen);
        presentation.skip_taskbar = application
            .skip_taskbar
            .unwrap_or(presentation.skip_taskbar);
        presentation.skip_pager = application.skip_pager.unwrap_or(presentation.skip_pager);
        if let Some(maximized) = application.maximized {
            (initially_maximized_horizontal, initially_maximized_vertical) = maximized.axes();
        }
        let session_identity = if metadata.leader.is_none_or(|leader| leader == window) {
            session_identity_from_parts(
                metadata.session_id,
                metadata.command,
                &application_identity,
            )
        } else {
            self.read_session_identity(window, &application_identity)?
        };
        let restored = session_identity
            .as_ref()
            .and_then(|identity| self.session_restore.take_match(identity));
        let policy = apply_size_capabilities(
            apply_application_decorations(client_policy, application.decorated),
            size_hints,
        );
        if map
            && self.clients.showing_desktop()
            && !self.show_desktop_strict
            && role_occupies_placement_space(policy.role)
        {
            self.set_showing_desktop(false, self.last_timestamp)?;
        }
        let mut initial_layer = application.layer.map_or(client_layer, application_layer);
        let rule_workspace = application.workspace.map(|workspace| match workspace {
            ApplicationWorkspace::All => WorkspaceAssignment::All,
            ApplicationWorkspace::Index(workspace) => {
                WorkspaceAssignment::Workspace(WorkspaceId::new(workspace.get() - 1))
            }
        });
        let mut focus_new = application.focus.unwrap_or(self.config.focus.focus_new);
        let decoration_override = restored
            .as_ref()
            .map_or(DecorationOverride::Default, |saved| {
                session_decoration_override(saved.decoration_override)
            });
        let effective_policy = policy.with_decoration_override(decoration_override);
        let mut workspace = relationships
            .transient_for
            .and_then(|transient| match transient {
                TransientTarget::Client(parent) => {
                    self.clients.get(parent).map(|parent| parent.workspace)
                }
                TransientTarget::Group => None,
            })
            .or_else(|| {
                metadata.desktop.and_then(|workspace| {
                    workspace_assignment_from_ewmh(workspace, self.clients.workspace_count())
                })
            })
            .or_else(|| {
                startup_sequence.as_ref().and_then(|sequence| {
                    sequence.1.desktop.and_then(|workspace| {
                        workspace_assignment_from_ewmh(workspace, self.clients.workspace_count())
                    })
                })
            })
            .unwrap_or(
                if matches!(policy.role, ClientRole::Desktop | ClientRole::Dock) {
                    WorkspaceAssignment::All
                } else {
                    WorkspaceAssignment::Workspace(self.clients.current_workspace())
                },
            );
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
        let requested_output_coverage = legacy_output_coverage(
            requested_geometry,
            policy,
            initially_maximized_horizontal || initially_maximized_vertical,
            initially_fullscreen,
            &self.outputs,
            self.root_geometry,
        );
        let mut content_geometry =
            if map && !normal_hints.positioned && role_occupies_placement_space(policy.role) {
                self.initial_placement(
                    constrained,
                    policy,
                    relationships.transient_for,
                    placement_assignment,
                )
            } else if map
                && normal_hints.positioned
                && policy.role == ClientRole::Normal
                && requested_output_coverage.is_none()
            {
                self.work_area_adjusted_origin(
                    requested_geometry,
                    effective_policy,
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
        } else if let Some(rule_workspace) = rule_workspace {
            workspace = rule_workspace;
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
        if metadata.desktop.is_some() {
            self.explicit_desktop_clients.insert(id);
        }
        self.application_identities.insert(id, application_identity);
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

        if let Some((token, _)) = startup_sequence.as_ref()
            && self.agent_launch_pending.remove(token)
        {
            self.agent_launch_tokens.insert(id, token.clone());
        }
        self.register_agent_client(id);
        let frame = self.create_frame(
            window,
            content_geometry,
            effective_policy,
            geometry.border_width,
            attributes.map_state != MapState::UNMAPPED,
        )?;
        self.frames.insert(id, frame);
        self.grab_client_mouse_bindings(id)?;
        self.refresh_client_opacity(window)?;
        if restored.is_none() && (application.position.is_some() || application.size.is_some()) {
            let position = application.position.unwrap_or_default();
            let size = application.size.unwrap_or_default();
            let apply_position = position.force || !normal_hints.positioned;
            self.apply_absolute_geometry(
                id,
                AbsoluteGeometryRequest {
                    x: position.x.filter(|_| apply_position),
                    y: position.y.filter(|_| apply_position),
                    width: size.width,
                    height: size.height,
                    width_basis: size.width_basis,
                    height_basis: size.height_basis,
                    output: if apply_position {
                        position.output
                    } else {
                        OutputTarget::Current
                    },
                },
            )?;
        }
        self.initialize_client_shape(window)?;
        self.refresh_user_time_window_from(window, metadata.user_time_window)?;
        self.refresh_client_colormaps(window)?;
        self.refresh_sync_counter(window)?;
        self.refresh_frame_colors(id)?;
        self.apply_title(window, initial_title)?;
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
        if !initially_iconic && self.clients.is_visible(id) {
            self.map_frame(window, frame)?;
        }

        if is_new {
            self.enforce_client_layer(id)?;
            self.restore_session_stacking(id)?;
            info!(window = format_args!("{window:#x}"), "managing X11 client");
        }
        let focus_candidate = focus_new
            && !initially_iconic
            && self.clients.is_visible(id)
            && policy.capabilities.focusable;
        if focus_candidate {
            if restored.as_ref().is_some_and(|saved| saved.focused)
                || self.focus_request_allowed(id, user_time, false, application.focus == Some(true))
            {
                if map {
                    self.pending_new_focus = Some(id);
                } else {
                    self.focus(window, self.last_timestamp)?;
                }
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
        if self.pending_new_focus == Some(id) {
            self.pending_new_focus = None;
        }
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
        if withdrawn && geometry.is_some() {
            let _ = window_request_succeeded(
                self.connection
                    .ungrab_button(ButtonIndex::ANY, window, ModMask::ANY)?
                    .check(),
            )?;
        }
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
        self.application_identities.remove(&id);
        self.explicit_desktop_clients.remove(&id);
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
        self.frame_sync.get_mut().remove(&id);
        if let Some(frame) = self.frames.remove(&id) {
            self.frame_parts.remove(&frame.window);
            if let Some(minimize_button) = frame.minimize_button {
                self.forget_frame_button(minimize_button);
            }
            if let Some(maximize_button) = frame.maximize_button {
                self.forget_frame_button(maximize_button);
            }
            if let Some(close_button) = frame.close_button {
                self.forget_frame_button(close_button);
            }
            for handle in frame.resize_handles.iter() {
                self.frame_parts.remove(&handle.window);
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
        if matches!(
            self.frame_parts.get(&event.event),
            Some(FramePart::Button(_, _))
        ) {
            self.set_hovered_frame_button(Some(event.event))?;
        }
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
                FramePart::Container(id)
                | FramePart::Button(id, _)
                | FramePart::ResizeHandle(id, _) => id,
            })
        };
        let Some(id) = id else {
            return Ok(());
        };
        self.last_timestamp = event.time;
        self.focus(window_id(id), event.time)?;
        Ok(())
    }

    fn leave_notify(&mut self, event: &LeaveNotifyEvent) -> Result<(), X11Error> {
        if self.hovered_frame_button == Some(event.event) {
            self.set_hovered_frame_button(None)?;
        }
        Ok(())
    }

    fn focus(&mut self, window: Window, timestamp: u32) -> Result<bool, X11Error> {
        self.focus_with_raise_policy(window, timestamp, FocusRaisePolicy::Configured)
    }

    fn finish_pending_new_focus(&mut self) -> Result<(), X11Error> {
        let Some(id) = self.pending_new_focus.take() else {
            return Ok(());
        };
        let _ = self.focus(window_id(id), self.last_timestamp)?;
        Ok(())
    }

    fn focus_with_raise_policy(
        &mut self,
        window: Window,
        timestamp: u32,
        raise_policy: FocusRaisePolicy,
    ) -> Result<bool, X11Error> {
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
        let previous = self.clients.focused();
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

        if previous != Some(id) {
            if let Some(previous) = previous
                && self.clients.contains(previous)
            {
                self.refresh_frame_colors(previous)?;
                self.draw_title(previous)?;
            }
            self.refresh_frame_colors(id)?;
            self.draw_title(id)?;
        }
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_ACTIVE_WINDOW,
            AtomEnum::WINDOW,
            &[window],
        )?;
        if raise_policy == FocusRaisePolicy::Configured && self.config.focus.raise_on_focus {
            self.raise_within_layer(id)?;
        } else {
            self.enforce_focus_dependent_layers()?;
        }
        Ok(true)
    }

    fn focus_fallback_from(&mut self, old: ClientId, timestamp: u32) -> Result<(), X11Error> {
        if self.clients.focused() != Some(old) {
            return Ok(());
        }
        let fallback = self.clients.focus_fallback_from(old);
        if let Some(fallback) = fallback
            && self.focus(window_id(fallback), timestamp)?
        {
            return Ok(());
        }
        self.realize_cleared_x_focus(Some(old), timestamp)
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
        let known = if self.clients.contains(client_id(event.event)) {
            Some(client_id(event.event))
        } else {
            self.frame_parts.get(&event.event).map(|part| match *part {
                FramePart::Container(id)
                | FramePart::Button(id, _)
                | FramePart::ResizeHandle(id, _) => id,
            })
        };
        if known.is_some_and(|id| self.published_focus.is_some_and(|focused| focused != id)) {
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
                    FramePart::Container(id)
                    | FramePart::Button(id, _)
                    | FramePart::ResizeHandle(id, _) => id,
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
            self.enforce_focus_dependent_layers()?;
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
            self.enforce_focus_dependent_layers()?;
        }
        Ok(())
    }

    fn clear_x_focus(&mut self, timestamp: u32) -> Result<(), X11Error> {
        let previous = self.clients.focused();
        self.realize_cleared_x_focus(previous, timestamp)
    }

    fn realize_cleared_x_focus(
        &mut self,
        previous: Option<ClientId>,
        timestamp: u32,
    ) -> Result<(), X11Error> {
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
            .set_input_focus(InputFocus::POINTER_ROOT, self.root, timestamp)?;
        self.connection
            .delete_property(self.root, self.atoms._NET_ACTIVE_WINDOW)?;
        if previous.is_some() {
            self.enforce_focus_dependent_layers()?;
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

    fn change_workspace_set(
        &mut self,
        placement: WorkspacePlacement,
        add: bool,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        let count = self.clients.workspace_count();
        if (add && usize::try_from(count).is_ok_and(|count| count >= MAX_WORKSPACES))
            || (!add && count <= 1)
        {
            return Ok(());
        }
        let workspace = match (placement, add) {
            (WorkspacePlacement::Current, _) => self.clients.current_workspace(),
            (WorkspacePlacement::Last, true) => WorkspaceId::new(count),
            (WorkspacePlacement::Last, false) => WorkspaceId::new(count - 1),
        };
        let Ok(index) = usize::try_from(workspace.index()) else {
            return Ok(());
        };

        self.finish_drag(timestamp)?;
        self.hide_menu(timestamp)?;
        let changed = if add {
            if !self.clients.insert_workspace(workspace) {
                return Ok(());
            }
            let name = self
                .config
                .workspaces
                .names
                .len()
                .saturating_add(1)
                .to_string();
            self.config.workspaces.names.insert(index, name);
            true
        } else if self.clients.remove_workspace(workspace) {
            self.config.workspaces.names.remove(index);
            true
        } else {
            false
        };
        if !changed {
            return Ok(());
        }

        self.refresh_workspace_layout()?;
        for id in self.clients.management_order() {
            if let Some(client) = self.clients.get(id) {
                self.publish_client_workspace(window_id(id), client.workspace)?;
            }
        }
        let _ = self.refresh_work_area()?;
        self.publish_workspaces()?;
        self.sync_workspace_visibility()?;
        self.restore_workspace_focus(timestamp)?;
        info!(
            workspaces = self.clients.workspace_count(),
            index = workspace.index() + 1,
            operation = if add { "added" } else { "removed" },
            "changed runtime workspace set"
        );
        Ok(())
    }

    fn set_showing_desktop(&mut self, showing: bool, timestamp: u32) -> Result<(), X11Error> {
        if !showing {
            self.show_desktop_strict = false;
        }
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
        // Skip the root property writes when the published value is already
        // identical; redundant writes only wake every listening pager.
        let managed_current = |published: &Option<Vec<u32>>| {
            published.as_ref().is_some_and(|list| {
                self.clients
                    .management_order()
                    .map(window_id)
                    .eq(list.iter().copied())
            })
        };
        if !managed_current(&self.published_client_list.borrow()) {
            let managed = self
                .clients
                .management_order()
                .map(window_id)
                .collect::<Vec<_>>();
            self.connection.change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                self.root,
                self.atoms._NET_CLIENT_LIST,
                AtomEnum::WINDOW,
                &managed,
            )?;
            *self.published_client_list.borrow_mut() = Some(managed);
        }
        let stacking_current = |published: &Option<Vec<u32>>| {
            published.as_ref().is_some_and(|list| {
                self.clients
                    .stacking()
                    .map(window_id)
                    .eq(list.iter().copied())
            })
        };
        if !stacking_current(&self.published_client_stacking.borrow()) {
            let stacking = self.clients.stacking().map(window_id).collect::<Vec<_>>();
            self.connection.change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                self.root,
                self.atoms._NET_CLIENT_LIST_STACKING,
                AtomEnum::WINDOW,
                &stacking,
            )?;
            *self.published_client_stacking.borrow_mut() = Some(stacking);
        }
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
        let stacking = self.clients.policy_stacking(&self.outputs);
        for id in stacking.iter().copied() {
            self.connection.configure_window(
                self.frame_window(id),
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )?;
        }
        self.clients.sync_stacking(stacking);
        self.update_client_lists()
    }

    fn enforce_client_layer(&mut self, id: ClientId) -> Result<(), X11Error> {
        let stacking = self.clients.policy_stacking(&self.outputs);
        let order_matches = self
            .clients
            .stacking()
            .filter(|candidate| *candidate != id)
            .eq(stacking
                .iter()
                .copied()
                .filter(|candidate| *candidate != id));
        if !order_matches {
            return self.enforce_layers();
        }
        let Some(index) = stacking.iter().position(|candidate| *candidate == id) else {
            return self.enforce_layers();
        };
        let values = if let Some(higher) = stacking.get(index.saturating_add(1)) {
            ConfigureWindowAux::new()
                .sibling(self.frame_window(*higher))
                .stack_mode(StackMode::BELOW)
        } else if let Some(lower) = index.checked_sub(1).and_then(|lower| stacking.get(lower)) {
            ConfigureWindowAux::new()
                .sibling(self.frame_window(*lower))
                .stack_mode(StackMode::ABOVE)
        } else {
            ConfigureWindowAux::new().stack_mode(StackMode::ABOVE)
        };
        self.connection
            .configure_window(self.frame_window(id), &values)?;
        self.clients.sync_stacking(stacking);
        self.update_client_lists()
    }

    fn raise_within_layer(&mut self, id: ClientId) -> Result<(), X11Error> {
        if !self.clients.raise(id) {
            return Ok(());
        }
        self.enforce_client_layer(id)
    }

    fn lower_within_layer(&mut self, id: ClientId) -> Result<(), X11Error> {
        if !self.clients.lower(id) {
            return Ok(());
        }
        self.enforce_client_layer(id)
    }

    fn raise_lower(&mut self, id: ClientId) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let Some(layer) = self.clients.effective_stacking_layer(id, &self.outputs) else {
            return Ok(());
        };
        let extents = self
            .frames
            .get(&id)
            .map_or_else(DecorationExtents::default, |frame| frame.extents);
        let geometry = visible_outer_geometry(client, extents);
        let stacking = self.clients.stacking().filter_map(|candidate| {
            let client = self.clients.get(candidate).copied()?;
            if client.iconic
                || !self.clients.is_visible(candidate)
                || self
                    .clients
                    .effective_stacking_layer(candidate, &self.outputs)
                    != Some(layer)
                || (candidate != id && self.clients.clients_share_transient_family(id, candidate))
            {
                return None;
            }
            let extents = self
                .frames
                .get(&candidate)
                .map_or_else(DecorationExtents::default, |frame| frame.extents);
            Some((candidate, visible_outer_geometry(client, extents)))
        });
        match adaptive_restack(id, geometry, stacking) {
            RestackDecision::Unchanged => Ok(()),
            RestackDecision::Raise => self.raise_within_layer(id),
            RestackDecision::Lower => self.lower_within_layer(id),
        }
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
            Event::LeaveNotify(event) => self.leave_notify(&event)?,
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
            Event::Expose(event) if event.count == 0 => {
                if let Some(FramePart::Container(id)) = self.frame_parts.get(&event.window).copied()
                {
                    self.draw_title(id)?;
                } else if matches!(
                    self.frame_parts.get(&event.window),
                    Some(FramePart::Button(_, _))
                ) {
                    self.draw_frame_button(event.window)?;
                } else if Some(event.window) == self.agent_indicator {
                    self.draw_agent_indicator()?;
                } else if self
                    .agent_consent
                    .as_ref()
                    .is_some_and(|consent| consent.window == event.window)
                {
                    self.draw_agent_consent()?;
                } else if event.window == self.focus_overlay.window {
                    self.draw_focus_overlay()?;
                } else if self.menu_overlay_is_visible(event.window) {
                    self.draw_menu_overlay_window(event.window)?;
                }
            }
            Event::Expose(_) => {}
            Event::SelectionClear(event) if event.selection == self.wm_selection => {
                warn!("lost the ICCCM window-manager selection");
                self.running = false;
            }
            Event::SelectionClear(event)
                if event.selection == self.agent_selection
                    && self
                        .agent_seat_ownership
                        .as_ref()
                        .is_some_and(|ownership| ownership.window == event.owner) =>
            {
                warn!("lost the Agent Seat provider selection; disabling only the agent seat");
                self.stop_agent_seat();
            }
            Event::SelectionClear(event) if event.selection == self.atoms.CLIPBOARD => {
                self.agent_text_selection_lost();
            }
            Event::SelectionRequest(event) if event.selection == self.wm_selection => {
                self.wm_selection_request(&event)?;
            }
            Event::SelectionRequest(event) if event.selection == self.atoms.CLIPBOARD => {
                self.agent_text_selection_request(&event)?;
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
                if event.atom == self.atoms._NET_WM_WINDOW_OPACITY
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.refresh_client_opacity(event.window)?;
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
                if event.atom == self.atoms._NET_STARTUP_ID
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.refresh_client_startup_sequence(event.window, event.time)?;
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
                if event.type_ == self.atoms._NET_STARTUP_INFO_BEGIN
                    || event.type_ == self.atoms._NET_STARTUP_INFO =>
            {
                self.startup_message_event(&event)?;
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
                self.explicit_desktop_clients
                    .insert(client_id(event.window));
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
        // Handled before drags, menus, chains, and anything else: the kill
        // chord must work while an agent session is flooding the manager.
        let chord = (
            event.detail,
            u16::from(event.state) & 0xff & !self.ignored_modifiers,
        );
        if !self.agent_kill_chord.is_empty() && self.agent_kill_chord.contains(&chord) {
            return self.toggle_agent_freeze();
        }
        // While the consent dialog is up it owns the keyboard: no binding, and
        // nothing an agent can do, competes with the person answering it.
        if self.agent_consent_key(event)? {
            return Ok(());
        }
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
        if self
            .focus_cycle
            .as_ref()
            .is_some_and(|cycle| cycle.modifiers == 0)
            && self.menu_keycodes.enter.contains(&event.detail)
        {
            self.finish_focus_cycle(event.time)?;
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
        let target = self.clients.focused();
        self.run_actions(actions, target, modifiers, event.time, None)?;
        Ok(())
    }

    fn action_query_context(&self, id: ClientId) -> Option<ActionQueryContext<'_>> {
        let client = self.clients.get(id)?;
        let identity = self.application_identities.get(&id)?;
        let output = self.outputs.output_for(client.geometry).id;
        let output = self
            .outputs
            .outputs()
            .iter()
            .position(|candidate| candidate.id == output)
            .and_then(|index| u32::try_from(index.saturating_add(1)).ok())?;
        Some(ActionQueryContext {
            identity: identity.as_application_identity(),
            workspace: match client.workspace {
                WorkspaceAssignment::Workspace(workspace) => Some(workspace.index()),
                WorkspaceAssignment::All => None,
            },
            active_workspace: self.clients.current_workspace().index(),
            last_workspace: self.clients.last_workspace().index(),
            output,
            shaded: client.shaded,
            maximized_horizontal: client.maximize.is_some_and(|maximize| maximize.horizontal),
            maximized_vertical: client.maximize.is_some_and(|maximize| maximize.vertical),
            minimized: client.iconic,
            fullscreen: client.fullscreen.is_some(),
            focused: self.clients.focused() == Some(id),
            focusable: client.policy.capabilities.focusable,
            urgent: client.presentation.urgent,
            decorated: client.policy.decorations.titlebar,
        })
    }

    fn action_queries_match(&self, queries: &[ActionQuery], target: Option<ClientId>) -> bool {
        let active_workspace = self.clients.current_workspace().index();
        queries.iter().all(|query| {
            let id = match query.target {
                ActionQueryTarget::Action => target,
                ActionQueryTarget::Focused => self.clients.focused(),
            };
            query.matches(
                id.and_then(|id| self.action_query_context(id)),
                active_workspace,
            )
        })
    }

    fn run_actions(
        &mut self,
        actions: Vec<Action>,
        target: Option<ClientId>,
        modifiers: u16,
        timestamp: u32,
        pointer: Option<PointerInvocation>,
    ) -> Result<ActionFlow, X11Error> {
        for action in actions {
            if self.run_action(action, target, modifiers, timestamp, pointer)? == ActionFlow::Stop {
                return Ok(ActionFlow::Stop);
            }
        }
        Ok(ActionFlow::Continue)
    }

    fn run_action(
        &mut self,
        action: Action,
        target: Option<ClientId>,
        modifiers: u16,
        timestamp: u32,
        pointer: Option<PointerInvocation>,
    ) -> Result<ActionFlow, X11Error> {
        if !matches!(
            &action,
            Action::NextWindow | Action::PreviousWindow | Action::CycleDirection { .. }
        ) {
            self.finish_focus_cycle(timestamp)?;
        }
        match action {
            Action::Execute {
                command,
                prompt,
                startup_notify,
            } => {
                let prepared = self.prepare_execute(command, startup_notify, target, pointer)?;
                if let Some(prompt) = prompt {
                    self.show_execute_prompt(prompt, prepared, timestamp)?;
                } else {
                    self.execute_prepared(prepared, timestamp)?;
                }
            }
            Action::LaunchTerminal => {
                let command = self.config.commands.terminal.clone();
                let prepared = self.prepare_execute(command, None, target, pointer)?;
                self.execute_prepared(prepared, timestamp)?;
            }
            Action::Screenshot { target: capture } => {
                let command = match capture {
                    ScreenshotTarget::Screen => self.config.commands.screenshot.clone(),
                    ScreenshotTarget::Window => self.config.commands.window_screenshot.clone(),
                };
                let prepared = self.prepare_execute(command, None, target, pointer)?;
                self.execute_prepared(prepared, timestamp)?;
            }
            Action::ShowMenu { menu } => {
                self.show_menu(&menu, target, pointer, timestamp)?;
            }
            Action::Reconfigure => {
                self.request_reconfigure()?;
            }
            Action::Restart { command } => {
                self.finish_drag(timestamp)?;
                self.disposition = RunDisposition::Restart { command };
                self.running = false;
            }
            Action::SessionLogout { prompt } => {
                self.finish_drag(timestamp)?;
                if !self.config.commands.session.trim().is_empty() {
                    let command = self.config.commands.session.clone();
                    let prepared = self.prepare_execute(command, None, target, pointer)?;
                    self.execute_prepared(prepared, timestamp)?;
                } else if prompt {
                    self.show_session_logout_prompt(timestamp)?;
                } else {
                    self.session_logout_requested = true;
                }
            }
            Action::Debug { message } => {
                info!(debug_message = %message, "debug action");
            }
            Action::If {
                queries,
                then_actions,
                else_actions,
            } => {
                let actions = if self.action_queries_match(&queries, target) {
                    then_actions
                } else {
                    else_actions
                };
                return self.run_actions(actions, target, modifiers, timestamp, pointer);
            }
            Action::ForEach {
                queries,
                then_actions,
                else_actions,
                none,
            } => {
                let clients: Vec<ClientId> = self.clients.management_order().collect();
                let mut matched = false;
                for id in clients {
                    if self.clients.get(id).is_none() {
                        continue;
                    }
                    let matches = self.action_queries_match(&queries, Some(id));
                    matched |= matches;
                    let actions = if matches {
                        then_actions.clone()
                    } else {
                        else_actions.clone()
                    };
                    if self.run_actions(actions, Some(id), modifiers, timestamp, pointer)?
                        == ActionFlow::Stop
                    {
                        break;
                    }
                }
                if !matched {
                    self.run_actions(none, target, modifiers, timestamp, pointer)?;
                }
            }
            Action::Stop => return Ok(ActionFlow::Stop),
            Action::Close => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.close_client(target, timestamp)?;
                }
            }
            Action::Kill => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    info!(
                        client = target.raw(),
                        "disconnecting X11 client by explicit action"
                    );
                    self.disconnect_client(target)?;
                }
            }
            Action::Focus { here } => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.activate_client(target, timestamp, here)?;
                }
            }
            Action::FocusToBottom => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.clients.focus_to_bottom(target);
                }
            }
            Action::Unfocus | Action::FocusFallback => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.focus_fallback_from(target, timestamp)?;
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
            Action::RaiseLower => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.raise_lower(target)?;
                }
            }
            Action::Minimize => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.iconify(window_id(target))?;
                }
            }
            Action::Maximize { direction } => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.set_maximize_direction(target, direction, true)?;
                }
            }
            Action::Unmaximize { direction } => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.set_maximize_direction(target, direction, false)?;
                }
            }
            Action::ToggleMaximize => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.toggle_full_maximize(target)?;
                }
            }
            Action::ToggleMaximizeHorizontal => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.toggle_maximize_axis(target, MaximizeAxis::Horizontal)?;
                }
            }
            Action::ToggleMaximizeVertical => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.toggle_maximize_axis(target, MaximizeAxis::Vertical)?;
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
            Action::SendToLayer { layer } => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    let layer = match layer {
                        LayerTarget::Below => ClientLayer::Below,
                        LayerTarget::Normal => ClientLayer::Normal,
                        LayerTarget::Above => ClientLayer::Above,
                    };
                    self.set_client_layer(window_id(target), layer)?;
                }
            }
            Action::Decorate => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.apply_decoration_override(target, DecorationOverride::Default)?;
                }
            }
            Action::Undecorate => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.apply_decoration_override(target, DecorationOverride::Undecorated)?;
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
            Action::Shade => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.set_shaded(window_id(target), true)?;
                }
            }
            Action::Unshade => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.set_shaded(window_id(target), false)?;
                }
            }
            Action::ToggleShade => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(client) = self.clients.get(target)
                {
                    self.set_shaded(window_id(target), !client.shaded)?;
                }
            }
            Action::ShadeLower => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(client) = self.clients.get(target)
                {
                    if client.shaded {
                        self.lower_within_layer(target)?;
                    } else {
                        self.set_shaded(window_id(target), true)?;
                    }
                }
            }
            Action::UnshadeRaise => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(client) = self.clients.get(target)
                {
                    if client.shaded {
                        self.set_shaded(window_id(target), false)?;
                    } else {
                        self.raise_within_layer(target)?;
                    }
                }
            }
            Action::ToggleShowDesktop { strict } => {
                let showing = !self.clients.showing_desktop();
                self.show_desktop_strict = showing && strict;
                self.set_showing_desktop(showing, timestamp)?;
            }
            Action::Move => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    if let Some(pointer) = pointer {
                        self.start_drag(target, DragKind::Move, pointer, timestamp)?;
                    } else {
                        self.start_keyboard_drag(target, DragKind::Move, timestamp)?;
                    }
                }
            }
            Action::Resize { edge } => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(operations) =
                        self.clients.get(target).map(|client| client.operations())
                {
                    let kind = if operations.resizable {
                        DragKind::Resize(pointer.map_or_else(
                            ResizeEdges::bottom_right,
                            |pointer| {
                                edge.map_or_else(
                                    || self.resize_edges(target, pointer),
                                    configured_resize_edges,
                                )
                            },
                        ))
                    } else if operations.movable {
                        DragKind::Move
                    } else {
                        return Ok(ActionFlow::Continue);
                    };
                    if let Some(pointer) = pointer {
                        self.start_drag(target, kind, pointer, timestamp)?;
                    } else {
                        self.start_keyboard_drag(target, kind, timestamp)?;
                    }
                }
            }
            Action::MoveRelative { x, y } => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(client) = self.clients.get(target).copied()
                    && client.operations().movable
                {
                    let bounds = self.available_geometry(target);
                    let geometry = client
                        .geometry
                        .translated(x.resolve(bounds.width), y.resolve(bounds.height))
                        .clamp_position(bounds);
                    self.configure_managed_geometry(
                        target,
                        GeometryRequest {
                            x: Some(geometry.x),
                            y: Some(geometry.y),
                            width: None,
                            height: None,
                            gravity: client.gravity,
                        },
                    )?;
                }
            }
            Action::ResizeRelative {
                left,
                right,
                top,
                bottom,
            } => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(client) = self.clients.get(target).copied()
                    && client.operations().resizable
                {
                    let geometry = relative_resize_geometry(
                        client.geometry,
                        ResizeDeltas {
                            left: left.resolve(client.geometry.width),
                            right: right.resolve(client.geometry.width),
                            top: top.resolve(client.geometry.height),
                            bottom: bottom.resolve(client.geometry.height),
                        },
                        client.size_hints,
                    );
                    self.configure_managed_geometry(
                        target,
                        GeometryRequest {
                            x: Some(geometry.x),
                            y: Some(geometry.y),
                            width: Some(geometry.width),
                            height: Some(geometry.height),
                            gravity: client.gravity,
                        },
                    )?;
                }
            }
            Action::MoveToEdge { direction } => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(field) = self.edge_action_field(target)
                    && field.client.operations().movable
                {
                    let geometry = directional_move_geometry(
                        field.geometry,
                        field.bounds,
                        &field.obstacles,
                        cardinal_direction(direction),
                    );
                    self.configure_managed_geometry(
                        target,
                        GeometryRequest {
                            x: Some(add_root_offset(geometry.x, field.extents.left)),
                            y: Some(add_root_offset(geometry.y, field.extents.top)),
                            width: None,
                            height: None,
                            gravity: field.client.gravity,
                        },
                    )?;
                }
            }
            Action::GrowToEdge { direction } => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(field) = self.edge_action_field(target)
                    && field.client.operations().resizable
                    && !(field.client.shaded && edge_direction_is_vertical(direction))
                {
                    let direction = cardinal_direction(direction);
                    let desired = directional_grow_geometry(
                        field.geometry,
                        field.bounds,
                        &field.obstacles,
                        direction,
                        BlockingEdgePolicy::Cross,
                    );
                    if !self.apply_edge_resize(target, &field, desired)?
                        && let Some(field) = self.edge_action_field(target)
                    {
                        let desired = directional_shrink_geometry(
                            field.geometry,
                            field.bounds,
                            &field.obstacles,
                            direction,
                        );
                        self.apply_edge_resize(target, &field, desired)?;
                    }
                }
            }
            Action::GrowToFill => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(field) = self.edge_action_field(target)
                    && field.client.operations().resizable
                    && !field.client.shaded
                {
                    let desired =
                        grow_to_fill_geometry(field.geometry, field.bounds, &field.obstacles);
                    self.apply_fill_resize(target, &field, desired)?;
                }
            }
            Action::ShrinkToEdge { direction } => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(field) = self.edge_action_field(target)
                    && field.client.operations().resizable
                    && !(field.client.shaded && edge_direction_is_vertical(direction))
                {
                    let desired = directional_shrink_geometry(
                        field.geometry,
                        field.bounds,
                        &field.obstacles,
                        cardinal_direction(direction),
                    );
                    self.apply_edge_resize(target, &field, desired)?;
                }
            }
            Action::MoveResizeTo {
                x,
                y,
                width,
                height,
                width_basis,
                height_basis,
                output,
            } => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.apply_absolute_geometry(
                        target,
                        AbsoluteGeometryRequest {
                            x,
                            y,
                            width,
                            height,
                            width_basis,
                            height_basis,
                            output,
                        },
                    )?;
                }
            }
            Action::MoveToCenter { output } => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.apply_absolute_geometry(
                        target,
                        AbsoluteGeometryRequest {
                            x: Some(AxisPosition::Center),
                            y: Some(AxisPosition::Center),
                            width: None,
                            height: None,
                            width_basis: SizeBasis::Outer,
                            height_basis: SizeBasis::Outer,
                            output,
                        },
                    )?;
                }
            }
            Action::FocusDirection { direction } => {
                self.focus_direction(target, direction, timestamp)?;
            }
            Action::CycleDirection { direction } => {
                self.cycle_focus_directional(direction, modifiers, timestamp)?;
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
            Action::LastWorkspace => {
                self.switch_workspace(self.clients.last_workspace(), timestamp)?;
            }
            Action::AddWorkspace { at } => {
                self.change_workspace_set(at, true, timestamp)?;
            }
            Action::RemoveWorkspace { at } => {
                self.change_workspace_set(at, false, timestamp)?;
            }
            Action::WorkspaceLeft { wrap } => {
                let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Left, wrap)?;
                self.switch_workspace(workspace, timestamp)?;
            }
            Action::WorkspaceRight { wrap } => {
                let workspace =
                    self.workspace_in_grid_direction(WorkspaceDirection::Right, wrap)?;
                self.switch_workspace(workspace, timestamp)?;
            }
            Action::WorkspaceUp { wrap } => {
                let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Up, wrap)?;
                self.switch_workspace(workspace, timestamp)?;
            }
            Action::WorkspaceDown { wrap } => {
                let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Down, wrap)?;
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
            Action::MoveToLastWorkspace { follow } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(self.clients.last_workspace()),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceLeft { follow, wrap } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    let workspace =
                        self.workspace_in_grid_direction(WorkspaceDirection::Left, wrap)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceRight { follow, wrap } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    let workspace =
                        self.workspace_in_grid_direction(WorkspaceDirection::Right, wrap)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceUp { follow, wrap } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    let workspace =
                        self.workspace_in_grid_direction(WorkspaceDirection::Up, wrap)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceDown { follow, wrap } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    let workspace =
                        self.workspace_in_grid_direction(WorkspaceDirection::Down, wrap)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::Exit { prompt } => {
                self.finish_drag(timestamp)?;
                if prompt {
                    self.show_exit_prompt(timestamp)?;
                } else {
                    self.disposition = RunDisposition::Exit;
                    self.running = false;
                }
            }
        }
        Ok(ActionFlow::Continue)
    }

    fn prepare_execute(
        &self,
        command: String,
        startup_notify: Option<StartupNotification>,
        target: Option<ClientId>,
        pointer: Option<PointerInvocation>,
    ) -> Result<PreparedExecute, X11Error> {
        self.prepare_execute_command(
            PreparedCommand::Shell(command),
            startup_notify,
            target,
            pointer,
        )
    }

    fn prepare_execute_command(
        &self,
        command: PreparedCommand,
        startup_notify: Option<StartupNotification>,
        target: Option<ClientId>,
        pointer: Option<PointerInvocation>,
    ) -> Result<PreparedExecute, X11Error> {
        let needs_pointer = match &command {
            PreparedCommand::Shell(command) => has_execute_variable(command, b"pointer"),
            PreparedCommand::Direct(_) => false,
        };
        let (pointer_x, pointer_y) = if !needs_pointer {
            (0, 0)
        } else if let Some(pointer) = pointer {
            (pointer.root_x, pointer.root_y)
        } else {
            let pointer = self.connection.query_pointer(self.root)?.reply()?;
            (pointer.root_x, pointer.root_y)
        };
        Ok(PreparedExecute {
            command,
            startup_notify,
            target,
            pointer_x,
            pointer_y,
        })
    }

    fn execute_prepared(
        &mut self,
        prepared: PreparedExecute,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        let PreparedExecute {
            command,
            startup_notify,
            target,
            pointer_x,
            pointer_y,
        } = prepared;
        let needs_pid = match &command {
            PreparedCommand::Shell(command) => has_execute_variable(command, b"pid"),
            PreparedCommand::Direct(_) => false,
        };
        let launched_by = target
            .filter(|id| self.clients.contains(*id))
            .map(window_id);
        let pid = if needs_pid && let Some(window) = launched_by {
            match self.read_cardinal_property(window, self.atoms._NET_WM_PID) {
                Ok(pid) => pid.unwrap_or(0),
                Err(error) if error.is_vanished_window() => 0,
                Err(error) => return Err(error),
            }
        } else {
            0
        };
        let (command_text, mut process) = match command {
            PreparedCommand::Shell(command) => {
                let command_text = expand_execute_variables(
                    &command,
                    pid,
                    launched_by.unwrap_or(NONE),
                    pointer_x,
                    pointer_y,
                );
                let mut process = Command::new("/bin/sh");
                process.arg("-c").arg(&command_text);
                (command_text, process)
            }
            PreparedCommand::Direct(command) => {
                let (program, arguments) = command
                    .argv()
                    .split_first()
                    .expect("desktop launch commands contain an executable");
                let mut process = if command.requires_terminal() {
                    let terminal = env::var_os("TERMINAL")
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| "xterm".into());
                    let mut process = Command::new(terminal);
                    process.arg("-e").arg(program).args(arguments);
                    process
                } else {
                    let mut process = Command::new(program);
                    process.args(arguments);
                    process
                };
                if let Some(directory) = command.working_directory() {
                    process.current_dir(directory);
                }
                (command.argv().join(" "), process)
            }
        };
        let startup_id = if let Some(notification) = startup_notify.as_ref() {
            Some(self.begin_startup_notification(
                &command_text,
                notification,
                launched_by,
                timestamp,
            )?)
        } else {
            None
        };
        process
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(startup_id) = startup_id.as_deref() {
            process.env("DESKTOP_STARTUP_ID", startup_id);
        }
        match process.spawn() {
            Ok(child) => {
                info!(
                    pid = child.id(),
                    command = command_text,
                    startup_id,
                    "started binding command"
                );
                if let Err(error) = self.process_reaper.watch(child) {
                    warn!(%error, "could not retain child process for reaping");
                }
            }
            Err(error) => {
                warn!(%error, command = command_text, "could not start binding command");
                if let Some(startup_id) = startup_id {
                    self.complete_startup_notification(&startup_id)?;
                }
            }
        }
        Ok(())
    }

    fn begin_startup_notification(
        &mut self,
        command: &str,
        notification: &StartupNotification,
        launched_by: Option<Window>,
        timestamp: u32,
    ) -> Result<String, X11Error> {
        let sequence = STARTUP_SEQUENCE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let startup_id = format!("nobox-{}-{sequence}_TIME{timestamp}", std::process::id());
        let program = startup_program(command);
        let name = notification.name.as_deref().unwrap_or(&program);
        let desktop = self.clients.current_workspace().index();
        let mut message = format!(
            "new: ID={} NAME={} SCREEN={} BIN={} DESKTOP={} TIMESTAMP={} DESCRIPTION={}",
            startup_value(&startup_id),
            startup_value(name),
            self.screen_index,
            startup_value(&program),
            desktop,
            timestamp,
            startup_value(&format!("Launching {name}")),
        );
        if let Some(icon) = notification.icon.as_deref() {
            message.push_str(" ICON=");
            message.push_str(&startup_value(icon));
        }
        if let Some(wm_class) = notification.wm_class.as_deref() {
            message.push_str(" WMCLASS=");
            message.push_str(&startup_value(wm_class));
        }
        if let Some(window) = launched_by {
            message.push_str(&format!(" LAUNCHED_BY={window}"));
        }
        self.send_startup_message(&message)?;
        self.startup_generation = self.startup_generation.wrapping_add(1);
        let generation = self.startup_generation;
        self.startup_sequences.insert(
            startup_id.clone(),
            StartupSequence {
                name: Some(name.to_owned()),
                binary: Some(program),
                wm_class: notification.wm_class.clone(),
                desktop: Some(desktop),
                timestamp: Some(timestamp),
                generation,
                initiated: true,
            },
        );
        self.runtime_timer
            .arm_startup(generation, STARTUP_SEQUENCE_TIMEOUT)?;
        Ok(startup_id)
    }

    fn complete_startup_notification(&mut self, startup_id: &str) -> Result<(), X11Error> {
        if self.startup_sequences.remove(startup_id).is_some() {
            self.send_startup_message(&format!("remove: ID={}", startup_value(startup_id)))?;
        }
        Ok(())
    }

    fn send_startup_message(&self, message: &str) -> Result<(), X11Error> {
        let source = self.connection.generate_id()?;
        self.connection
            .create_window(
                0,
                source,
                self.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new(),
            )?
            .check()?;
        let mut bytes = message.as_bytes().to_vec();
        bytes.push(0);
        for (index, chunk) in bytes.chunks(20).enumerate() {
            let mut data = [0_u8; 20];
            data[..chunk.len()].copy_from_slice(chunk);
            let atom = if index == 0 {
                self.atoms._NET_STARTUP_INFO_BEGIN
            } else {
                self.atoms._NET_STARTUP_INFO
            };
            let event = ClientMessageEvent::new(8, source, atom, data);
            self.connection
                .send_event(false, self.root, EventMask::PROPERTY_CHANGE, event)?
                .check()?;
        }
        self.connection.destroy_window(source)?.check()?;
        self.connection.flush()?;
        Ok(())
    }

    fn startup_message_event(&mut self, event: &ClientMessageEvent) -> Result<(), X11Error> {
        if event.format != 8 {
            return Ok(());
        }
        if event.type_ == self.atoms._NET_STARTUP_INFO_BEGIN {
            if self.startup_message_buffers.len() >= MAX_STARTUP_MESSAGE_BUFFERS
                && !self.startup_message_buffers.contains_key(&event.window)
                && let Some(oldest) = self.startup_message_buffers.keys().next().copied()
            {
                self.startup_message_buffers.remove(&oldest);
            }
            self.startup_message_buffers
                .insert(event.window, Vec::new());
        } else if event.type_ != self.atoms._NET_STARTUP_INFO
            || !self.startup_message_buffers.contains_key(&event.window)
        {
            return Ok(());
        }
        let data = event.data.as_data8();
        let terminator = data.iter().position(|byte| *byte == 0);
        let payload = terminator.map_or(data.as_slice(), |index| &data[..index]);
        let Some(buffer) = self.startup_message_buffers.get_mut(&event.window) else {
            return Ok(());
        };
        if buffer.len().saturating_add(payload.len()) > MAX_STARTUP_MESSAGE_BYTES {
            self.startup_message_buffers.remove(&event.window);
            warn!(
                source = event.window,
                "discarded oversized startup-notification message"
            );
            return Ok(());
        }
        buffer.extend_from_slice(payload);
        if terminator.is_none() {
            return Ok(());
        }
        let message = self
            .startup_message_buffers
            .remove(&event.window)
            .unwrap_or_default();
        let Ok(message) = String::from_utf8(message) else {
            warn!(
                source = event.window,
                "discarded non-UTF-8 startup notification"
            );
            return Ok(());
        };
        let Some(message) = parse_startup_message(&message) else {
            warn!(
                source = event.window,
                "discarded malformed startup notification"
            );
            return Ok(());
        };
        self.apply_startup_message(message)
    }

    fn apply_startup_message(&mut self, message: ParsedStartupMessage) -> Result<(), X11Error> {
        if message.kind == StartupMessageKind::Remove {
            self.startup_sequences.remove(&message.id);
            return Ok(());
        }
        let is_new_sequence = !self.startup_sequences.contains_key(&message.id);
        if is_new_sequence
            && self.startup_sequences.len() >= MAX_STARTUP_SEQUENCES
            && let Some(oldest) = self.startup_sequences.keys().next().cloned()
        {
            self.startup_sequences.remove(&oldest);
        }
        self.startup_generation = self.startup_generation.wrapping_add(1);
        let generation = self.startup_generation;
        let id_timestamp = startup_timestamp(&message.id);
        let sequence = self.startup_sequences.entry(message.id).or_default();
        if let Some(name) = message.fields.get("NAME") {
            sequence.name = Some(name.clone());
        }
        if let Some(binary) = message.fields.get("BIN") {
            sequence.binary = Some(binary.clone());
        }
        if let Some(wm_class) = message.fields.get("WMCLASS") {
            sequence.wm_class = Some(wm_class.clone());
        }
        if let Some(desktop) = message
            .fields
            .get("DESKTOP")
            .and_then(|desktop| desktop.parse().ok())
        {
            sequence.desktop = Some(desktop);
        }
        if let Some(timestamp) = message
            .fields
            .get("TIMESTAMP")
            .and_then(|timestamp| timestamp.parse().ok())
            .or(id_timestamp)
        {
            sequence.timestamp = Some(timestamp);
        }
        if message.kind == StartupMessageKind::New {
            sequence.initiated = true;
        }
        let arm_timeout = is_new_sequence;
        if arm_timeout {
            sequence.generation = generation;
            let timeout = if message.kind == StartupMessageKind::Change {
                STARTUP_CHANGE_TIMEOUT
            } else {
                STARTUP_SEQUENCE_TIMEOUT
            };
            self.runtime_timer.arm_startup(generation, timeout)?;
        }
        Ok(())
    }

    /// Finds and completes the startup sequence a new window belongs to,
    /// returning its identifier as well: that identifier is the correlation
    /// token an agent was given when it asked for the launch.
    fn match_startup_sequence(
        &mut self,
        startup_id: Option<&str>,
        application: &X11ApplicationIdentity,
    ) -> Result<Option<(String, StartupSequence)>, X11Error> {
        let startup_id = if let Some(startup_id) = startup_id {
            self.startup_sequences
                .get(startup_id)
                .is_some_and(|sequence| sequence.initiated)
                .then(|| startup_id.to_owned())
        } else {
            self.startup_sequences.iter().find_map(|(id, sequence)| {
                let class_match = sequence.wm_class.as_deref().is_some_and(|wm_class| {
                    wm_class == application.name || wm_class == application.class
                });
                let binary_match = sequence.binary.as_deref().is_some_and(|binary| {
                    binary.eq_ignore_ascii_case(&application.name)
                        || binary.eq_ignore_ascii_case(&application.class)
                });
                (sequence.initiated && (class_match || binary_match)).then(|| id.clone())
            })
        };
        let Some(startup_id) = startup_id else {
            return Ok(None);
        };
        let sequence = self.startup_sequences.get(&startup_id).cloned();
        self.complete_startup_notification(&startup_id)?;
        Ok(sequence.map(|sequence| (startup_id, sequence)))
    }

    fn refresh_client_startup_sequence(
        &mut self,
        window: Window,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        let startup_id = self.read_startup_id(window)?;
        let Some(application) = self.application_identities.get(&client_id(window)).cloned() else {
            return Ok(());
        };
        let Some((_, sequence)) =
            self.match_startup_sequence(startup_id.as_deref(), &application)?
        else {
            return Ok(());
        };
        if !self.explicit_desktop_clients.contains(&client_id(window))
            && let Some(workspace) = sequence.desktop.and_then(|workspace| {
                workspace_assignment_from_ewmh(workspace, self.clients.workspace_count())
            })
        {
            self.move_to_workspace(client_id(window), workspace, timestamp, false)?;
        }
        Ok(())
    }

    fn edge_action_field(&self, target: ClientId) -> Option<EdgeActionField> {
        let client = self.clients.get(target).copied()?;
        let extents = self
            .frames
            .get(&target)
            .map_or_else(DecorationExtents::default, |frame| frame.extents);
        let geometry = visible_outer_geometry(client, extents);
        let bounds = extents.outer_geometry(self.available_geometry(target));
        let obstacles = self
            .clients
            .stacking()
            .filter(|candidate| {
                *candidate != target
                    && self.clients.is_visible(*candidate)
                    && self
                        .clients
                        .get(*candidate)
                        .is_some_and(|client| !client.iconic)
            })
            .filter_map(|candidate| {
                let client = self.clients.get(candidate)?;
                let extents = self
                    .frames
                    .get(&candidate)
                    .map_or_else(DecorationExtents::default, |frame| frame.extents);
                Some(visible_outer_geometry(*client, extents))
            })
            .collect();
        Some(EdgeActionField {
            client,
            extents,
            geometry,
            bounds,
            obstacles,
        })
    }

    fn apply_edge_resize(
        &mut self,
        target: ClientId,
        field: &EdgeActionField,
        desired: Geometry,
    ) -> Result<bool, X11Error> {
        let geometry = relative_resize_geometry(
            field.client.geometry,
            ResizeDeltas::between(field.geometry, desired),
            field.client.size_hints,
        );
        self.configure_managed_geometry(
            target,
            GeometryRequest {
                x: Some(geometry.x),
                y: Some(geometry.y),
                width: Some(geometry.width),
                height: Some(geometry.height),
                gravity: field.client.gravity,
            },
        )?;
        Ok(self
            .clients
            .get(target)
            .is_some_and(|client| client.geometry != field.client.geometry))
    }

    fn apply_fill_resize(
        &mut self,
        target: ClientId,
        field: &EdgeActionField,
        desired: Geometry,
    ) -> Result<(), X11Error> {
        let geometry = field.extents.content_geometry(desired);
        self.configure_managed_geometry(
            target,
            GeometryRequest {
                x: Some(geometry.x),
                y: Some(geometry.y),
                width: Some(geometry.width),
                height: Some(geometry.height),
                gravity: field.client.gravity,
            },
        )
    }

    fn apply_absolute_geometry(
        &mut self,
        target: ClientId,
        request: AbsoluteGeometryRequest,
    ) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(target).copied() else {
            return Ok(());
        };
        let operations = client.operations();
        let wants_resize = request.width.is_some() || request.height.is_some();
        if !(operations.movable || operations.resizable && wants_resize) {
            return Ok(());
        }
        let extents = self
            .frames
            .get(&target)
            .map_or_else(DecorationExtents::default, |frame| frame.extents);
        let current = extents.outer_geometry(client.geometry);
        let workspace = match client.workspace {
            WorkspaceAssignment::Workspace(workspace) => workspace,
            WorkspaceAssignment::All => self.clients.current_workspace(),
        };
        let current_output = self.outputs.output_for(current);
        let pointer_output = if request.output == OutputTarget::Pointer {
            let pointer = self.connection.query_pointer(self.root)?.reply()?;
            Some(self.outputs.output_for(Geometry::new(
                i32::from(pointer.root_x),
                i32::from(pointer.root_y),
                1,
                1,
            )))
        } else {
            None
        };
        let source_bounds = self
            .output_work_areas
            .get(&(current_output.id, workspace))
            .copied()
            .unwrap_or(current_output.geometry);
        let target_bounds = if operations.movable {
            match resolve_output_target(
                &self.outputs,
                current_output,
                pointer_output,
                request.output,
            ) {
                Some(PlacementOutput::Output(output)) => self
                    .output_work_areas
                    .get(&(output.id, workspace))
                    .copied()
                    .unwrap_or(output.geometry),
                Some(PlacementOutput::All) => self
                    .work_areas
                    .get(usize::try_from(workspace.index()).unwrap_or(usize::MAX))
                    .copied()
                    .unwrap_or(self.root_geometry),
                None => {
                    warn!(?request.output, "absolute geometry action selected a missing output");
                    return Ok(());
                }
            }
        } else {
            source_bounds
        };
        let requested = Size::new(
            requested_content_dimension(
                request.width.filter(|_| operations.resizable),
                request.width_basis,
                target_bounds.width,
                client.geometry.width,
                extents.left.saturating_add(extents.right),
            ),
            requested_content_dimension(
                request.height.filter(|_| operations.resizable),
                request.height_basis,
                target_bounds.height,
                client.geometry.height,
                extents.top.saturating_add(extents.bottom),
            ),
        );
        let titlebar_height = extents.top.saturating_sub(extents.left);
        let constrained = x_content_size(client.size_hints.constrain(requested), titlebar_height);
        let outer_size =
            extents.outer_geometry(Geometry::new(0, 0, constrained.width, constrained.height));
        let mut outer = move_resize_geometry(
            current,
            source_bounds,
            target_bounds,
            Size::new(outer_size.width, outer_size.height),
            request.x.map_or(AxisPlacement::Keep, |position| {
                axis_placement(position, target_bounds.width)
            }),
            request.y.map_or(AxisPlacement::Keep, |position| {
                axis_placement(position, target_bounds.height)
            }),
        );
        if !operations.movable {
            outer.x = current.x;
            outer.y = current.y;
        }
        let geometry = extents.content_geometry(outer);
        self.configure_managed_geometry(
            target,
            GeometryRequest {
                x: Some(geometry.x),
                y: Some(geometry.y),
                width: Some(geometry.width),
                height: Some(geometry.height),
                gravity: client.gravity,
            },
        )
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
        let Some(cycle) = self.close_focus_cycle(timestamp)? else {
            return Ok(());
        };
        if let Some(selected) = cycle
            .index
            .and_then(|index| cycle.candidates.get(index))
            .copied()
        {
            self.activate_focus_cycle_target(selected, timestamp)?;
        }
        Ok(())
    }

    fn cancel_focus_cycle(&mut self, timestamp: u32) -> Result<(), X11Error> {
        let original = self
            .close_focus_cycle(timestamp)?
            .and_then(|cycle| cycle.original);
        if let Some(original) = original {
            self.focus(window_id(original), timestamp)?;
        }
        Ok(())
    }

    fn close_focus_cycle(&mut self, timestamp: u32) -> Result<Option<FocusCycle>, X11Error> {
        let cycle = self.focus_cycle.take();
        let visuals_result = self.hide_focus_cycle_visuals();
        let keyboard_result = cycle
            .as_ref()
            .is_some_and(|cycle| cycle.keyboard_grabbed)
            .then(|| self.connection.ungrab_keyboard(timestamp))
            .transpose();
        visuals_result?;
        keyboard_result?;
        Ok(cycle)
    }

    fn update_focus_overlay(&mut self) -> Result<(), X11Error> {
        if !self.config.switcher.enabled {
            return self.hide_focus_cycle_visuals();
        }
        let (index, selected, candidate_count) = {
            let Some(cycle) = self.focus_cycle.as_ref() else {
                return self.hide_focus_cycle_visuals();
            };
            let Some(index) = cycle.index else {
                return self.hide_focus_cycle_visuals();
            };
            let Some(selected) = cycle.candidates.get(index).copied() else {
                return self.hide_focus_cycle_visuals();
            };
            if !cycle.keyboard_grabbed {
                return self.hide_focus_cycle_visuals();
            }
            (index, selected, cycle.candidates.len())
        };
        self.update_focus_indicator(selected)?;
        let output = self.clients.get(selected).map_or_else(
            || self.outputs.primary(),
            |client| self.outputs.output_for(client.geometry),
        );
        let available_height = output.geometry.height.saturating_sub(40).max(1);
        let fitting_rows = (available_height / self.config.switcher.row_height).max(1);
        let rows = candidate_count.min(
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
        let start = focus_cycle_visible_start(candidate_count, index, rows);
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.focus_overlay.window,
            self.atoms._NOBOX_FOCUS_SWITCHER,
            AtomEnum::CARDINAL,
            &[
                window_id(selected),
                u32::try_from(index).unwrap_or(u32::MAX),
                u32::try_from(candidate_count).unwrap_or(u32::MAX),
                u32::try_from(start).unwrap_or(u32::MAX),
            ],
        )?;
        if !self.focus_overlay.mapped {
            self.connection.map_window(self.focus_overlay.window)?;
            self.focus_overlay.mapped = true;
        }
        self.draw_focus_overlay()
    }

    fn update_focus_indicator(&mut self, selected: ClientId) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(selected).copied() else {
            return self.hide_focus_indicator();
        };
        let extents = self
            .frames
            .get(&selected)
            .map_or_else(DecorationExtents::default, |frame| frame.extents);
        let outer = visible_outer_geometry(client, extents);
        for (window, geometry) in self
            .focus_indicator
            .windows
            .into_iter()
            .zip(focus_indicator_geometries(outer))
        {
            self.connection.configure_window(
                window,
                &ConfigureWindowAux::new()
                    .x(geometry.x)
                    .y(geometry.y)
                    .width(geometry.width)
                    .height(geometry.height)
                    .stack_mode(StackMode::ABOVE),
            )?;
        }
        if !self.focus_indicator.mapped {
            for window in self.focus_indicator.windows {
                self.connection.map_window(window)?;
            }
            self.focus_indicator.mapped = true;
        }
        Ok(())
    }

    fn hide_focus_cycle_visuals(&mut self) -> Result<(), X11Error> {
        let indicator_result = self.hide_focus_indicator();
        let overlay_result = self.hide_focus_overlay();
        indicator_result?;
        overlay_result
    }

    fn hide_focus_indicator(&mut self) -> Result<(), X11Error> {
        if self.focus_indicator.mapped {
            for window in self.focus_indicator.windows {
                self.connection.unmap_window(window)?;
            }
            self.focus_indicator.mapped = false;
        }
        Ok(())
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
            let (text, _) = fitted_title_text(
                title,
                self.focus_overlay.width.saturating_sub(24),
                255,
                &self.title_font.metrics,
            );
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
                text_baseline(row_y, row_height, &self.title_font.metrics),
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
        self.show_runtime_menu(runtime_menu, target, pointer, timestamp)
    }

    fn show_session_logout_prompt(&mut self, timestamp: u32) -> Result<(), X11Error> {
        self.hide_menu(timestamp)?;
        let runtime_menu = RuntimeMenu {
            id: "__nobox_session_logout".to_owned(),
            title: "End the session?".to_owned(),
            entries: vec![
                runtime_internal_action("_Cancel", RuntimeMenuAction::Dismiss),
                runtime_internal_action("_Log out", RuntimeMenuAction::SessionLogout),
            ],
        };
        self.show_runtime_menu(runtime_menu, None, None, timestamp)
    }

    fn show_exit_prompt(&mut self, timestamp: u32) -> Result<(), X11Error> {
        self.hide_menu(timestamp)?;
        let runtime_menu = RuntimeMenu {
            id: "__nobox_exit".to_owned(),
            title: "Exit nobox?".to_owned(),
            entries: vec![
                runtime_internal_action("_Cancel", RuntimeMenuAction::Dismiss),
                runtime_internal_action("_Exit", RuntimeMenuAction::Exit),
            ],
        };
        self.show_runtime_menu(runtime_menu, None, None, timestamp)
    }

    fn show_execute_prompt(
        &mut self,
        prompt: String,
        prepared: PreparedExecute,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        self.hide_menu(timestamp)?;
        let runtime_menu = RuntimeMenu {
            id: "__nobox_execute_prompt".to_owned(),
            title: prompt,
            entries: vec![
                runtime_internal_action("_Cancel", RuntimeMenuAction::Dismiss),
                runtime_internal_action("_Execute", RuntimeMenuAction::Execute(prepared)),
            ],
        };
        self.show_runtime_menu(runtime_menu, None, None, timestamp)
    }

    fn show_runtime_menu(
        &mut self,
        runtime_menu: RuntimeMenu,
        target: Option<ClientId>,
        pointer: Option<PointerInvocation>,
        timestamp: u32,
    ) -> Result<(), X11Error> {
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
        let output = self
            .outputs
            .output_for(Geometry::new(anchor_x, anchor_y, 1, 1));
        let runtime_menu = paginate_runtime_menu(
            runtime_menu,
            menu_row_capacity(
                output.geometry.height,
                self.config.menu.row_height,
                self.config.menu.max_rows,
            ),
        );
        let Some(selected) = first_selectable_menu_entry(&runtime_menu.entries) else {
            return Ok(());
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
                menu = runtime_menu.id,
                "could not retain pointer grab for menu"
            );
            return Ok(());
        }
        let keyboard_status = self
            .connection
            .grab_keyboard(
                false,
                self.root,
                CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )?
            .reply()?
            .status;
        if keyboard_status != GrabStatus::SUCCESS {
            self.connection.ungrab_pointer(timestamp)?;
            warn!(
                ?keyboard_status,
                menu = runtime_menu.id,
                "could not retain keyboard grab for menu"
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
            MenuSource::Command => {
                let command = definition.command.as_deref()?;
                let output = match command_menu_output(
                    command,
                    Duration::from_millis(u64::from(self.config.menu.command_timeout_ms)),
                ) {
                    Ok(output) => output,
                    Err(error) => {
                        warn!(menu = definition.id, %error, "command menu failed");
                        return None;
                    }
                };
                let entries = match self.config.parse_command_menu(&definition.id, &output) {
                    Ok(entries) => entries,
                    Err(error) => {
                        warn!(menu = definition.id, %error, "command menu returned invalid TOML");
                        return None;
                    }
                };
                Some(RuntimeMenu {
                    id: definition.id.clone(),
                    title: definition.title.clone(),
                    entries: entries.iter().map(runtime_configured_entry).collect(),
                })
            }
            MenuSource::Applications => self.resolve_applications_menu(definition),
            MenuSource::Client => self.resolve_client_menu(definition, target?),
            MenuSource::ClientWorkspaces => {
                self.resolve_client_workspaces_menu(definition, target?)
            }
            MenuSource::Windows => self.resolve_windows_menu(definition),
        }
    }

    fn resolve_applications_menu(&self, definition: &MenuDefinition) -> Option<RuntimeMenu> {
        const MAX_DYNAMIC_ENTRIES: usize = 1_024;
        let catalog = &self.application_catalog;
        debug!(
            applications = catalog.application_count(),
            skipped = catalog.skipped_files(),
            "discovered XDG application menu"
        );
        let mut entries = Vec::with_capacity(catalog.groups().len());
        let mut remaining = MAX_DYNAMIC_ENTRIES;
        for (index, group) in catalog.groups().iter().enumerate() {
            if remaining <= 1 {
                break;
            }
            let application_count = group.applications.len().min(remaining - 1);
            if application_count == 0 {
                continue;
            }
            let title = group.category.title();
            let category = RuntimeMenu {
                id: format!("{}:category:{index}", definition.id),
                title: title.to_owned(),
                entries: group.applications[..application_count]
                    .iter()
                    .cloned()
                    .map(runtime_application)
                    .collect(),
            };
            entries.push(runtime_inline_submenu(title, category));
            remaining = remaining.saturating_sub(application_count.saturating_add(1));
        }
        if entries.is_empty() {
            entries.push(runtime_internal_action(
                "No applications found",
                RuntimeMenuAction::Dismiss,
            ));
        }
        Some(RuntimeMenu {
            id: definition.id.clone(),
            title: definition.title.clone(),
            entries,
        })
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
                    follow: true,
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

    fn create_menu_overlay(&self) -> Result<MenuOverlay, X11Error> {
        let window = self.connection.generate_id()?;
        self.connection
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                0,
                0,
                1,
                1,
                x_u16(self.config.theme.border_width.clamp(1, 8)),
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .background_pixel(self.decoration_pixels.inactive_titlebar)
                    .border_pixel(self.decoration_pixels.active_border)
                    .cursor(self.cursors.pointer)
                    .override_redirect(1_u32)
                    .save_under(1_u32)
                    .event_mask(EventMask::EXPOSURE),
            )?
            .check()?;
        self.connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms._NET_WM_NAME,
            self.atoms.UTF8_STRING,
            b"nobox:menu",
        )?;
        self.connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"nobox-menu\0nobox\0",
        )?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            &[self.atoms._NET_WM_WINDOW_TYPE_MENU],
        )?;
        Ok(MenuOverlay {
            window,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            mapped: false,
        })
    }

    fn menu_overlay_is_visible(&self, window: Window) -> bool {
        self.menu_session.as_ref().is_some_and(|session| {
            self.menu_overlay.window == window
                || session
                    .parents
                    .iter()
                    .any(|parent| parent.overlay.window == window)
        })
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
        let Some(session) = self.menu_session.as_ref() else {
            return Ok(());
        };
        self.draw_menu_frame(&session.menu, session.selected, self.menu_overlay)
    }

    fn draw_menu_overlay_window(&self, window: Window) -> Result<(), X11Error> {
        let Some(session) = self.menu_session.as_ref() else {
            return Ok(());
        };
        if self.menu_overlay.window == window {
            return self.draw_menu_frame(&session.menu, session.selected, self.menu_overlay);
        }
        let Some(parent) = session
            .parents
            .iter()
            .find(|parent| parent.overlay.window == window)
        else {
            return Ok(());
        };
        self.draw_menu_frame(&parent.menu, parent.selected, parent.overlay)
    }

    fn draw_menu_frame(
        &self,
        definition: &RuntimeMenu,
        selected_entry: usize,
        overlay: MenuOverlay,
    ) -> Result<(), X11Error> {
        if !overlay.mapped {
            return Ok(());
        }
        let row_height = self.config.menu.row_height;
        let rows = definition.entries.len().min(
            usize::try_from(
                (overlay.height / row_height)
                    .saturating_sub(1)
                    .min(self.config.menu.max_rows),
            )
            .unwrap_or(usize::MAX),
        );
        let start = focus_cycle_visible_start(definition.entries.len(), selected_entry, rows);
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new().foreground(self.decoration_pixels.inactive_titlebar),
        )?;
        self.connection.poly_fill_rectangle(
            overlay.window,
            self.title_gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: x_dimension(overlay.width),
                height: x_dimension(overlay.height),
            }],
        )?;
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new().foreground(self.decoration_pixels.active_titlebar),
        )?;
        self.connection.poly_fill_rectangle(
            overlay.window,
            self.title_gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: x_dimension(overlay.width),
                height: x_dimension(row_height),
            }],
        )?;
        self.draw_menu_text(
            overlay,
            &definition.title,
            12,
            0,
            self.decoration_pixels.active_titlebar,
        )?;

        for (row, entry) in definition.entries[start..start + rows].iter().enumerate() {
            let index = start + row;
            let y =
                row_height.saturating_mul(u32::try_from(row.saturating_add(1)).unwrap_or(u32::MAX));
            let selected = index == selected_entry && menu_entry_is_selectable(entry);
            let background = if selected {
                self.decoration_pixels.active_titlebar
            } else {
                self.decoration_pixels.inactive_titlebar
            };
            if selected {
                self.connection
                    .change_gc(self.title_gc, &ChangeGCAux::new().foreground(background))?;
                self.connection.poly_fill_rectangle(
                    overlay.window,
                    self.title_gc,
                    &[Rectangle {
                        x: 0,
                        y: clamp_i16_u32(y),
                        width: x_dimension(overlay.width),
                        height: x_dimension(row_height),
                    }],
                )?;
            }
            match entry {
                RuntimeMenuEntry::Item { label, .. } => {
                    self.draw_menu_text(overlay, label, 12, y, background)?;
                }
                RuntimeMenuEntry::Submenu { label, .. } => {
                    let label = format!("{label}  >");
                    self.draw_menu_text(overlay, &label, 12, y, background)?;
                }
                RuntimeMenuEntry::Separator { label } => {
                    self.connection.change_gc(
                        self.title_gc,
                        &ChangeGCAux::new().foreground(self.decoration_pixels.inactive_border),
                    )?;
                    self.connection.poly_fill_rectangle(
                        overlay.window,
                        self.title_gc,
                        &[Rectangle {
                            x: 8,
                            y: clamp_i16_u32(y.saturating_add(row_height / 2)),
                            width: x_dimension(overlay.width.saturating_sub(16)),
                            height: 1,
                        }],
                    )?;
                    if let Some(label) = label {
                        self.draw_menu_text(overlay, label, 12, y, background)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn draw_menu_text(
        &self,
        overlay: MenuOverlay,
        text: &str,
        x: i16,
        row_y: u32,
        background: u32,
    ) -> Result<(), X11Error> {
        let (text, _) = fitted_title_text(
            text,
            overlay.width.saturating_sub(24),
            255,
            &self.title_font.metrics,
        );
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new()
                .foreground(self.decoration_pixels.title_text)
                .background(background),
        )?;
        self.connection.image_text8(
            overlay.window,
            self.title_gc,
            x,
            text_baseline(row_y, self.config.menu.row_height, &self.title_font.metrics),
            &text,
        )?;
        Ok(())
    }

    fn menu_pointer_motion(&mut self, root_x: i16, root_y: i16) -> Result<(), X11Error> {
        let Some((level, index)) = self.menu_entry_at(root_x, root_y) else {
            return Ok(());
        };
        let Some((selected, entry)) = self.menu_frame_entry(level, index) else {
            return Ok(());
        };
        if !menu_entry_is_selectable(&entry) {
            return Ok(());
        }
        let active_level = self
            .menu_session
            .as_ref()
            .map_or(0, |session| session.parents.len());
        if level < active_level && selected == index {
            return Ok(());
        }
        self.leave_submenus_after(level)?;
        if let Some(session) = &mut self.menu_session {
            session.selected = index;
            session.pending_key = None;
        }
        self.update_menu_overlay()?;
        if let RuntimeMenuEntry::Submenu { menu, .. } = entry {
            self.enter_submenu(index, menu)?;
        }
        Ok(())
    }

    fn menu_button_press(&mut self, event: &ButtonPressEvent) -> Result<(), X11Error> {
        self.last_timestamp = event.time;
        let Some((level, index)) = self.menu_entry_at(event.root_x, event.root_y) else {
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
            .is_some_and(|session| level == session.parents.len() && session.selected != index)
        {
            self.draw_menu_overlay()?;
        }
        Ok(())
    }

    fn menu_button_release(&mut self, event: &ButtonReleaseEvent) -> Result<(), X11Error> {
        self.last_timestamp = event.time;
        self.set_pressed_frame_button(None)?;
        if self
            .menu_session
            .as_ref()
            .is_some_and(|session| session.opening_button == Some(event.detail))
        {
            if let Some(session) = &mut self.menu_session {
                session.opening_button = None;
            }
            let Some((level, index)) = self.menu_entry_at(event.root_x, event.root_y) else {
                return Ok(());
            };
            return self.activate_menu_entry(
                level,
                index,
                mouse_modifier_mask(u16::from(event.state)),
                event.time,
                None,
            );
        }
        if matches!(event.detail, 4 | 5) {
            return Ok(());
        }
        let Some((level, index)) = self.menu_entry_at(event.root_x, event.root_y) else {
            return self.hide_menu(event.time);
        };
        self.activate_menu_entry(
            level,
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
                    let level = self
                        .menu_session
                        .as_ref()
                        .map_or(0, |session| session.parents.len());
                    return self.activate_menu_entry(level, selected, 0, event.time, None);
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
                self.run_actions(actions, target, modifiers, timestamp, pointer)?;
                Ok(())
            }
            RuntimeMenuAction::ActivateClient(id) => self.activate_client_from_menu(id, timestamp),
            RuntimeMenuAction::Dismiss => Ok(()),
            RuntimeMenuAction::SessionLogout => {
                self.session_logout_requested = true;
                Ok(())
            }
            RuntimeMenuAction::Execute(prepared) => self.execute_prepared(prepared, timestamp),
            RuntimeMenuAction::LaunchApplication(application) => {
                let startup_notify = application.startup_notify.then(|| StartupNotification {
                    name: Some(application.name.clone()),
                    icon: application.icon.clone(),
                    wm_class: application.startup_wm_class.clone(),
                });
                let prepared = self.prepare_execute_command(
                    PreparedCommand::Direct(application.command),
                    startup_notify,
                    target,
                    pointer,
                )?;
                self.execute_prepared(prepared, timestamp)
            }
            RuntimeMenuAction::Exit => {
                self.disposition = RunDisposition::Exit;
                self.running = false;
                Ok(())
            }
        }
    }

    fn activate_client_from_menu(&mut self, id: ClientId, timestamp: u32) -> Result<(), X11Error> {
        self.activate_client(id, timestamp, false)
    }

    fn activate_client(
        &mut self,
        id: ClientId,
        timestamp: u32,
        here: bool,
    ) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        self.set_showing_desktop(false, timestamp)?;
        if let WorkspaceAssignment::Workspace(workspace) = client.workspace
            && workspace != self.clients.current_workspace()
        {
            if here {
                self.move_to_workspace(
                    id,
                    WorkspaceAssignment::Workspace(self.clients.current_workspace()),
                    timestamp,
                    false,
                )?;
            } else {
                self.switch_workspace(workspace, timestamp)?;
            }
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

    fn enter_submenu(&mut self, selected: usize, menu: RuntimeSubmenu) -> Result<(), X11Error> {
        let target = self
            .menu_session
            .as_ref()
            .and_then(|session| session.target);
        let runtime_menu = match menu {
            RuntimeSubmenu::Named(menu) => self.resolve_menu(&menu, target),
            RuntimeSubmenu::Inline(menu) => Some(*menu),
        };
        let Some(runtime_menu) = runtime_menu else {
            return Ok(());
        };
        let Some(session) = self.menu_session.as_ref() else {
            return Ok(());
        };
        let row_height = self.config.menu.row_height;
        let rows = session.menu.entries.len().min(
            usize::try_from(
                (self.menu_overlay.height / row_height)
                    .saturating_sub(1)
                    .min(self.config.menu.max_rows),
            )
            .unwrap_or(usize::MAX),
        );
        let start = focus_cycle_visible_start(session.menu.entries.len(), selected, rows);
        let visible_row = selected.saturating_sub(start);
        let preferred_y =
            self.menu_overlay.y.saturating_add(
                i32::try_from(row_height.saturating_mul(
                    u32::try_from(visible_row.saturating_add(1)).unwrap_or(u32::MAX),
                ))
                .unwrap_or(i32::MAX),
            );
        let output = self.outputs.output_for(Geometry::new(
            self.menu_overlay.x,
            self.menu_overlay.y,
            self.menu_overlay.width,
            self.menu_overlay.height,
        ));
        let width = self
            .config
            .menu
            .width
            .min(output.geometry.width.saturating_sub(20).max(1));
        let anchor_x = place_submenu_axis(
            self.menu_overlay.x,
            self.menu_overlay.width,
            output.geometry.x,
            output.geometry.width,
            width,
        );
        let available_height = output.geometry.height.saturating_sub(20).max(1);
        let fitting_rows = (available_height / row_height).saturating_sub(1).max(1);
        let child_capacity =
            usize::try_from(self.config.menu.max_rows.min(fitting_rows)).unwrap_or(usize::MAX);
        let runtime_menu = paginate_runtime_menu(runtime_menu, child_capacity);
        let Some(next) = first_selectable_menu_entry(&runtime_menu.entries) else {
            return Ok(());
        };
        let child_rows = runtime_menu.entries.len().min(child_capacity);
        let child_height = row_height
            .saturating_mul(u32::try_from(child_rows.saturating_add(1)).unwrap_or(u32::MAX))
            .min(available_height)
            .max(1);
        let anchor_y = clamp_popup_axis(
            preferred_y,
            output.geometry.y,
            output.geometry.height,
            child_height,
        );
        let overlay = self.create_menu_overlay()?;
        if let Some(session) = &mut self.menu_session {
            let parent = std::mem::replace(&mut session.menu, runtime_menu);
            session.parents.push(MenuParent {
                menu: parent,
                overlay: self.menu_overlay,
                selected,
                anchor_x: session.anchor_x,
                anchor_y: session.anchor_y,
                centered: session.centered,
            });
            session.selected = next;
            session.anchor_x = anchor_x;
            session.anchor_y = anchor_y;
            session.centered = false;
            session.pending_key = None;
        }
        self.menu_overlay = overlay;
        self.update_menu_overlay()
    }

    fn leave_submenu(&mut self) -> Result<(), X11Error> {
        let parent = self
            .menu_session
            .as_mut()
            .and_then(|session| session.parents.pop());
        if let Some(parent) = parent {
            if self.menu_overlay.mapped {
                self.connection.unmap_window(self.menu_overlay.window)?;
            }
            self.connection.destroy_window(self.menu_overlay.window)?;
            self.menu_overlay = parent.overlay;
            let Some(session) = &mut self.menu_session else {
                return Ok(());
            };
            session.menu = parent.menu;
            session.selected = parent.selected;
            session.anchor_x = parent.anchor_x;
            session.anchor_y = parent.anchor_y;
            session.centered = parent.centered;
            session.pending_key = None;
            self.update_menu_overlay()?;
        }
        Ok(())
    }

    fn leave_submenus_after(&mut self, level: usize) -> Result<(), X11Error> {
        while self
            .menu_session
            .as_ref()
            .is_some_and(|session| session.parents.len() > level)
        {
            self.leave_submenu()?;
        }
        Ok(())
    }

    fn activate_menu_entry(
        &mut self,
        level: usize,
        index: usize,
        modifiers: u16,
        timestamp: u32,
        pointer: Option<PointerInvocation>,
    ) -> Result<(), X11Error> {
        let Some((_, entry)) = self.menu_frame_entry(level, index) else {
            return Ok(());
        };
        let active_level = self
            .menu_session
            .as_ref()
            .map_or(0, |session| session.parents.len());
        if matches!(entry, RuntimeMenuEntry::Submenu { .. }) && level < active_level {
            return Ok(());
        }
        self.leave_submenus_after(level)?;
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

    fn menu_frame_entry(&self, level: usize, index: usize) -> Option<(usize, RuntimeMenuEntry)> {
        let session = self.menu_session.as_ref()?;
        let (menu, selected) = if level == session.parents.len() {
            (&session.menu, session.selected)
        } else {
            let parent = session.parents.get(level)?;
            (&parent.menu, parent.selected)
        };
        menu.entries
            .get(index)
            .cloned()
            .map(|entry| (selected, entry))
    }

    fn menu_entry_at(&self, root_x: i16, root_y: i16) -> Option<(usize, usize)> {
        let session = self.menu_session.as_ref()?;
        for level in (0..=session.parents.len()).rev() {
            let (menu, selected, overlay) = if level == session.parents.len() {
                (&session.menu, session.selected, self.menu_overlay)
            } else {
                let parent = &session.parents[level];
                (&parent.menu, parent.selected, parent.overlay)
            };
            if let Some(index) = menu_frame_entry_at(
                menu,
                selected,
                overlay,
                self.config.menu.row_height,
                self.config.menu.max_rows,
                root_x,
                root_y,
            ) {
                return Some((level, index));
            }
        }
        None
    }

    fn focus_direction(
        &mut self,
        action_target: Option<ClientId>,
        direction: WindowDirection,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        let candidates = self.clients.focus_cycle_candidates();
        let selected = self.directional_focus_candidate(
            action_target.or_else(|| self.clients.focused()),
            &candidates,
            direction,
        );
        let Some(selected) = selected else {
            return Ok(());
        };
        debug!(
            client = selected.raw(),
            ?direction,
            "selected spatial focus target"
        );
        self.activate_focus_cycle_target(selected, timestamp)
    }

    fn directional_focus_candidate(
        &self,
        origin: Option<ClientId>,
        candidates: &[ClientId],
        direction: WindowDirection,
    ) -> Option<ClientId> {
        let Some(origin) = origin else {
            return candidates.first().copied();
        };
        let client = self.clients.get(origin).copied()?;
        let origin_geometry = visible_outer_geometry(
            client,
            self.frames
                .get(&origin)
                .map_or_else(DecorationExtents::default, |frame| frame.extents),
        );
        let rectangles = candidates.iter().filter_map(|candidate| {
            let client = self.clients.get(*candidate).copied()?;
            let extents = self
                .frames
                .get(candidate)
                .map_or_else(DecorationExtents::default, |frame| frame.extents);
            Some((*candidate, visible_outer_geometry(client, extents)))
        });
        directional_target(
            origin,
            origin_geometry,
            rectangles,
            spatial_direction(direction),
        )
        .or_else(|| candidates.contains(&origin).then_some(origin))
    }

    fn activate_focus_cycle_target(
        &mut self,
        selected: ClientId,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        if self
            .clients
            .get(selected)
            .is_some_and(|client| client.shaded)
        {
            self.set_shaded(window_id(selected), false)?;
        }
        if self.focus(window_id(selected), timestamp)? && !self.config.focus.raise_on_focus {
            self.raise_within_layer(selected)?;
        }
        Ok(())
    }

    fn hide_menu(&mut self, timestamp: u32) -> Result<(), X11Error> {
        let session = self.menu_session.take();
        let root_overlay = session
            .as_ref()
            .and_then(|session| session.parents.first())
            .map_or(self.menu_overlay, |parent| parent.overlay);
        if let Some(session) = &session {
            for parent in session.parents.iter().skip(1) {
                if parent.overlay.mapped {
                    self.connection.unmap_window(parent.overlay.window)?;
                }
                self.connection.destroy_window(parent.overlay.window)?;
            }
            if !session.parents.is_empty() {
                if self.menu_overlay.mapped {
                    self.connection.unmap_window(self.menu_overlay.window)?;
                }
                self.connection.destroy_window(self.menu_overlay.window)?;
            }
        }
        self.menu_overlay = root_overlay;
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

    fn prepare_focus_cycle(
        &mut self,
        kind: FocusCycleKind,
        modifiers: u16,
        timestamp: u32,
    ) -> Result<bool, X11Error> {
        if self
            .focus_cycle
            .as_ref()
            .is_some_and(|cycle| cycle.kind == kind && cycle.modifiers == modifiers)
        {
            return Ok(true);
        }
        self.finish_focus_cycle(timestamp)?;
        let candidates = self.clients.focus_cycle_candidates();
        if candidates.is_empty() {
            return Ok(false);
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
            kind,
            candidates,
            index,
            original: self.clients.focused(),
            modifiers,
            keyboard_grabbed: status == GrabStatus::SUCCESS,
        });
        Ok(true)
    }

    fn cycle_focus(
        &mut self,
        direction: FocusCycleDirection,
        modifiers: u16,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        if !self.prepare_focus_cycle(FocusCycleKind::Linear, modifiers, timestamp)? {
            return Ok(());
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
            let focused = match self.focus_with_raise_policy(
                window_id(candidate),
                timestamp,
                FocusRaisePolicy::Suppress,
            ) {
                Ok(focused) => focused,
                Err(error) => {
                    if let Err(ungrab_error) = self.close_focus_cycle(timestamp) {
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

    fn cycle_focus_directional(
        &mut self,
        direction: WindowDirection,
        modifiers: u16,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        if !self.prepare_focus_cycle(FocusCycleKind::Spatial, modifiers, timestamp)? {
            return Ok(());
        }
        let selected = self.focus_cycle.as_ref().and_then(|cycle| {
            let origin = cycle
                .index
                .and_then(|index| cycle.candidates.get(index))
                .copied();
            self.directional_focus_candidate(origin, &cycle.candidates, direction)
        });
        let Some(selected) = selected else {
            return Ok(());
        };
        if let Some(cycle) = &mut self.focus_cycle {
            cycle.index = cycle
                .candidates
                .iter()
                .position(|candidate| *candidate == selected);
        }
        debug!(
            client = selected.raw(),
            ?direction,
            "advancing modifier-held spatial focus cycle"
        );
        let focused = match self.focus_with_raise_policy(
            window_id(selected),
            timestamp,
            FocusRaisePolicy::Suppress,
        ) {
            Ok(focused) => focused,
            Err(error) => {
                if let Err(ungrab_error) = self.close_focus_cycle(timestamp) {
                    warn!(%ungrab_error, "could not release failed spatial-cycle grab");
                }
                return Err(error);
            }
        };
        if focused {
            self.update_focus_overlay()?;
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
            self.disconnect_client(client)?;
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
            self.disconnect_client(client)?;
        }
        Ok(())
    }

    fn disconnect_client(&mut self, client: ClientId) -> Result<(), X11Error> {
        if self.pending_pings.remove(&client).is_some() {
            self.runtime_timer.cancel_ping(client)?;
        }
        self.connection.kill_client(window_id(client))?;
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
        let application = self.refresh_application_settings(window, client_policy.role)?;
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

    fn enforce_focus_dependent_layers(&mut self) -> Result<(), X11Error> {
        if self.clients.stacking().any(|id| {
            self.clients.get(id).is_some_and(|client| {
                client.fullscreen.is_some() || client.output_coverage.is_some()
            })
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
        wrap: Option<bool>,
    ) -> Result<WorkspaceId, X11Error> {
        self.refresh_workspace_layout()?;
        Ok(self
            .clients
            .workspace_in_grid_direction(direction, wrap.unwrap_or(self.config.workspaces.wrap)))
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
            let mut reservations = self
                .struts
                .iter()
                .filter_map(|(id, reservation)| {
                    self.clients
                        .get(*id)
                        .filter(|client| client.workspace.is_visible_on(workspace))
                        .map(|_| *reservation)
                })
                .collect::<Vec<_>>();
            let margins = configured_margin_reservations(&self.config, screen);
            if edge_reservations_are_nonempty(margins) {
                reservations.push(margins);
            }
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
            configured_margins = ?self.config.margins,
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

    fn set_maximize_direction(
        &mut self,
        id: ClientId,
        direction: MaximizeDirection,
        enabled: bool,
    ) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let mut horizontal = client.maximize.is_some_and(|maximize| maximize.horizontal);
        let mut vertical = client.maximize.is_some_and(|maximize| maximize.vertical);
        let previous = (horizontal, vertical);
        match direction {
            MaximizeDirection::Both => (horizontal, vertical) = (enabled, enabled),
            MaximizeDirection::Horizontal => horizontal = enabled,
            MaximizeDirection::Vertical => vertical = enabled,
        }
        if (horizontal, vertical) == previous {
            return Ok(());
        }
        self.set_maximized(window_id(id), horizontal, vertical)
    }

    fn toggle_maximize_axis(&mut self, id: ClientId, axis: MaximizeAxis) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let mut horizontal = client.maximize.is_some_and(|maximize| maximize.horizontal);
        let mut vertical = client.maximize.is_some_and(|maximize| maximize.vertical);
        match axis {
            MaximizeAxis::Horizontal => horizontal = !horizontal,
            MaximizeAxis::Vertical => vertical = !vertical,
        }
        self.set_maximized(window_id(id), horizontal, vertical)
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

    fn apply_decoration_override(
        &mut self,
        id: ClientId,
        preference: DecorationOverride,
    ) -> Result<(), X11Error> {
        if self.clients.get(id).is_none_or(|client| {
            !client.operations().decoratable || client.decoration_override == preference
        }) {
            return Ok(());
        }
        if self.clients.get(id).is_some_and(|client| client.shaded) {
            self.set_shaded(window_id(id), false)?;
        }
        if let Some(policy) = self.clients.set_decoration_override(id, preference) {
            self.apply_frame_policy(id, policy)?;
            self.publish_allowed_actions(id)?;
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
                self.forget_frame_button(button);
                self.connection.destroy_window(button)?;
                None
            }
            (None, true) => {
                let button = self.create_frame_button(
                    id,
                    previous.window,
                    geometry.width,
                    extents,
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
                self.forget_frame_button(button);
                self.connection.destroy_window(button)?;
                None
            }
            (None, true) => {
                let button = self.create_frame_button(
                    id,
                    previous.window,
                    geometry.width,
                    extents,
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
                self.forget_frame_button(button);
                self.connection.destroy_window(button)?;
                None
            }
            (None, true) => {
                let button = self.create_frame_button(
                    id,
                    previous.window,
                    geometry.width,
                    extents,
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
        self.connection
            .configure_window(previous.window, &ConfigureWindowAux::new().border_width(0))?;
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
        let frame_width = geometry
            .width
            .saturating_add(frame.extents.left)
            .saturating_add(frame.extents.right);
        let frame_height = if self.clients.get(id).is_some_and(|client| client.shaded) {
            frame.extents.top.saturating_add(frame.extents.bottom)
        } else {
            geometry
                .height
                .saturating_add(frame.extents.top)
                .saturating_add(frame.extents.bottom)
        };
        self.connection.configure_window(
            frame.window,
            &ConfigureWindowAux::new()
                .x(outer.x)
                .y(outer.y)
                .width(frame_width)
                .height(frame_height),
        )?;
        let sync_state = FrameSyncState {
            frame: frame.window,
            minimize_button: frame.minimize_button,
            maximize_button: frame.maximize_button,
            close_button: frame.close_button,
            extents_left: frame.extents.left,
            extents_top: frame.extents.top,
            width: geometry.width,
            height: geometry.height,
            frame_width,
            frame_height,
            handles_enabled: self.resize_handles_enabled(id),
        };
        let children_unchanged = self.frame_sync.borrow().get(&id) == Some(&sync_state);
        if !children_unchanged {
            self.configure_frame_children(id, frame, geometry, sync_state)?;
            self.frame_sync.borrow_mut().insert(id, sync_state);
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

    fn configure_frame_children(
        &self,
        id: ClientId,
        frame: Frame,
        geometry: Geometry,
        sync_state: FrameSyncState,
    ) -> Result<(), X11Error> {
        let titlebar_height = frame.extents.top.saturating_sub(frame.extents.left);
        self.connection.configure_window(
            window_id(id),
            &ConfigureWindowAux::new()
                .x(i32::try_from(frame.extents.left).unwrap_or(i32::MAX))
                .y(i32::try_from(frame.extents.top).unwrap_or(i32::MAX))
                .width(geometry.width)
                .height(geometry.height)
                .border_width(0),
        )?;
        if let Some(close_button) = frame.close_button {
            let size = titlebar_height.saturating_sub(8).max(1).min(geometry.width);
            self.connection.configure_window(
                close_button,
                &ConfigureWindowAux::new()
                    .x(add_root_offset(
                        button_x(geometry.width, size, 0),
                        frame.extents.left,
                    ))
                    .width(size)
                    .height(size),
            )?;
        }
        if let Some(maximize_button) = frame.maximize_button {
            let size = titlebar_height.saturating_sub(8).max(1).min(geometry.width);
            self.connection.configure_window(
                maximize_button,
                &ConfigureWindowAux::new()
                    .x(add_root_offset(
                        button_x(
                            geometry.width,
                            size,
                            u32::from(frame.close_button.is_some()),
                        ),
                        frame.extents.left,
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
                    .x(add_root_offset(
                        button_x(
                            geometry.width,
                            size,
                            u32::from(frame.close_button.is_some())
                                + u32::from(frame.maximize_button.is_some()),
                        ),
                        frame.extents.left,
                    ))
                    .width(size)
                    .height(size),
            )?;
        }
        self.sync_resize_handles(id, frame, sync_state.frame_width, sync_state.frame_height)
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
        if event.detail == u8::from(ButtonIndex::M1)
            && modifiers == 0
            && matches!(target.context, MouseContext::Root | MouseContext::Desktop)
        {
            self.clear_x_focus(event.time)?;
        }
        let pointer = PointerInvocation {
            target,
            button: event.detail,
            root_x: event.root_x,
            root_y: event.root_y,
        };
        if event.detail == u8::from(ButtonIndex::M1)
            && matches!(
                self.frame_parts.get(&target.window),
                Some(FramePart::Button(_, _))
            )
        {
            self.set_pressed_frame_button(Some(target.window))?;
        }
        self.dispatch_mouse_binding(
            target,
            event.detail,
            modifiers,
            MouseTrigger::Press,
            pointer,
            event.time,
        )?;
        if self.replays_client_press(target, event.detail, modifiers) {
            self.connection
                .allow_events(Allow::REPLAY_POINTER, event.time)?;
            self.dispatch_mouse_binding(
                target,
                event.detail,
                modifiers,
                MouseTrigger::Release,
                pointer,
                event.time,
            )?;
            self.finish_mouse_click(
                MouseClick {
                    target,
                    button: event.detail,
                    modifiers,
                    root_x: event.root_x,
                    root_y: event.root_y,
                    timestamp: event.time,
                },
                pointer,
            )?;
            return Ok(());
        }
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
        self.set_pressed_frame_button(None)?;
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
        self.finish_mouse_click(
            MouseClick {
                target: gesture.target,
                button: gesture.button,
                modifiers: gesture.modifiers,
                root_x: event.root_x,
                root_y: event.root_y,
                timestamp: event.time,
            },
            pointer,
        )
    }

    fn finish_mouse_click(
        &mut self,
        current: MouseClick,
        pointer: PointerInvocation,
    ) -> Result<(), X11Error> {
        self.dispatch_mouse_binding(
            current.target,
            current.button,
            current.modifiers,
            MouseTrigger::Click,
            pointer,
            current.timestamp,
        )?;
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
                current.target,
                current.button,
                current.modifiers,
                MouseTrigger::DoubleClick,
                pointer,
                current.timestamp,
            )?;
        } else {
            self.last_mouse_click = Some(current);
        }
        Ok(())
    }

    fn replays_client_press(&self, target: MouseTarget, button: u8, modifiers: u16) -> bool {
        modifiers == 0
            && target
                .client
                .is_some_and(|id| target.window == window_id(id))
            && matches!(target.context, MouseContext::Client | MouseContext::Desktop)
            && (self.has_mouse_binding(target.context, button, modifiers)
                || (target.context == MouseContext::Desktop && button == u8::from(ButtonIndex::M1)))
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
            Some(FramePart::ResizeHandle(id, part)) => {
                let Some(client) = self.clients.get(id) else {
                    return false;
                };
                let Some(frame) = self.frames.get(&id) else {
                    return false;
                };
                let frame_width = client
                    .geometry
                    .width
                    .saturating_add(frame.extents.left)
                    .saturating_add(frame.extents.right);
                let frame_height = if client.shaded {
                    frame.extents.top.saturating_add(frame.extents.bottom)
                } else {
                    client
                        .geometry
                        .height
                        .saturating_add(frame.extents.top)
                        .saturating_add(frame.extents.bottom)
                };
                let geometry =
                    resize_handle_geometry(part, frame_width, frame_height, frame.extents);
                (geometry.width, geometry.height, 0)
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
        self.run_actions(actions, target.client, modifiers, timestamp, Some(pointer))?;
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
                Some(FramePart::ResizeHandle(id, part)) => {
                    return MouseTarget {
                        client: Some(id),
                        context: part.context(),
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

    fn start_keyboard_drag(
        &mut self,
        id: ClientId,
        kind: DragKind,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        let pointer = self.connection.query_pointer(self.root)?.reply()?;
        self.begin_drag(
            id,
            DragStart {
                kind,
                pointer_x: pointer.root_x,
                pointer_y: pointer.root_y,
                button: None,
                keyboard: true,
                grab_pointer: true,
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
        let cursor = match start.kind {
            DragKind::Move => self.cursors.move_window,
            DragKind::Resize(edges) => self.cursors.for_resize(edges),
        };
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
                    cursor,
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
        if !start.grab_pointer {
            self.connection.change_active_pointer_grab(
                cursor,
                start.timestamp,
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
            )?;
        }
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
            DragKind::Move => {
                let requested = Geometry::new(
                    drag.initial.x.saturating_add(dx),
                    drag.initial.y.saturating_add(dy),
                    drag.initial.width,
                    drag.initial.height,
                );
                let requested = if self.config.mouse.snap_to_windows {
                    self.snap_move_to_visible_clients(id, requested, resistance)
                } else {
                    requested
                };
                requested.snap_movement(bounds, resistance)
            }
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

    fn snap_move_to_visible_clients(
        &self,
        id: ClientId,
        requested: Geometry,
        resistance: u32,
    ) -> Geometry {
        let Some(mut moving) = self.clients.get(id).copied() else {
            return requested;
        };
        moving.geometry = requested;
        let extents = self
            .frames
            .get(&id)
            .map_or_else(DecorationExtents::default, |frame| frame.extents);
        let outer = visible_outer_geometry(moving, extents);
        let targets = self
            .clients
            .stacking()
            .filter(|candidate| {
                *candidate != id
                    && self.clients.is_visible(*candidate)
                    && self.clients.get(*candidate).is_some_and(|client| {
                        !(client.iconic
                            || client.layer == ClientLayer::Below
                                && client.presentation.skip_taskbar)
                    })
            })
            .filter_map(|candidate| {
                let client = self.clients.get(candidate)?;
                let extents = self
                    .frames
                    .get(&candidate)
                    .map_or_else(DecorationExtents::default, |frame| frame.extents);
                Some(visible_outer_geometry(*client, extents))
            });
        let snapped = outer.snap_movement_to(targets, resistance);
        let content = extents.content_geometry(snapped);
        Geometry::new(content.x, content.y, requested.width, requested.height)
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
        } else {
            self.connection.change_active_pointer_grab(
                NONE,
                timestamp,
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
            )?;
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
        } else {
            self.connection.change_active_pointer_grab(
                NONE,
                timestamp,
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
            )?;
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
                .ungrab_button(ButtonIndex::ANY, window, ModMask::ANY)?
                .check(),
        )?;
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
        let _ = self.hide_menu(CURRENT_TIME);
        self.stop_agent_seat();
        let _ = self.finish_drag(self.last_timestamp);
        let clients: Vec<ClientId> = self.clients.management_order().collect();
        for id in clients {
            let _ = self.release_client_for_shutdown(id);
        }
        self.runtime_timer.stop();
        self.process_reaper.stop();
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
        for cursor in self.cursors.as_array() {
            let _ = self.connection.free_cursor(cursor);
        }
        for property in [
            self.atoms._NET_SUPPORTING_WM_CHECK,
            self.atoms._NET_SUPPORTED,
            self.atoms._NET_ACTIVE_WINDOW,
            self.atoms._NET_CLIENT_LIST,
            self.atoms._NET_CLIENT_LIST_STACKING,
            self.atoms._NET_SHOWING_DESKTOP,
            self.atoms._NET_WORKAREA,
        ] {
            if let Ok(cookie) = self.connection.delete_property(self.root, property) {
                let _ = cookie.check();
            }
        }
        let _ = self.connection.free_gc(self.title_gc);
        let _ = self.connection.close_font(self.title_font.id);
        let _ = self.connection.change_window_attributes(
            self.root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
        );
        let _ = self.connection.destroy_window(self.support_window);
        let _ = self.connection.flush();
    }
}

struct TitleFont {
    id: Font,
    metrics: FontMetrics,
}

#[derive(Clone)]
struct FontMetrics {
    advances: [u16; 256],
    ascent: u16,
    descent: u16,
}

impl FontMetrics {
    fn from_reply(reply: &QueryFontReply) -> Self {
        let fallback = font_character_info(reply, reply.default_char)
            .unwrap_or(&reply.max_bounds)
            .character_width
            .max(0);
        let advances = std::array::from_fn(|byte| {
            let width = font_character_info(reply, u16::try_from(byte).unwrap_or(0))
                .map_or(fallback, |info| info.character_width.max(0));
            u16::try_from(width).unwrap_or(0)
        });
        Self {
            advances,
            ascent: u16::try_from(reply.font_ascent.max(0)).unwrap_or(0),
            descent: u16::try_from(reply.font_descent.max(0)).unwrap_or(0),
        }
    }

    const fn advance(&self, byte: u8) -> u16 {
        self.advances[byte as usize]
    }
}

fn font_character_info(reply: &QueryFontReply, character: u16) -> Option<&Charinfo> {
    if reply.char_infos.is_empty() {
        return Some(&reply.max_bounds);
    }
    let byte1 = u8::try_from(character >> 8).ok()?;
    let byte2 = character & 0xff;
    if !(reply.min_byte1..=reply.max_byte1).contains(&byte1)
        || !(reply.min_char_or_byte2..=reply.max_char_or_byte2).contains(&byte2)
    {
        return None;
    }
    let columns = usize::from(
        reply
            .max_char_or_byte2
            .saturating_sub(reply.min_char_or_byte2)
            .saturating_add(1),
    );
    let row = usize::from(byte1.saturating_sub(reply.min_byte1));
    let column = usize::from(byte2.saturating_sub(reply.min_char_or_byte2));
    reply
        .char_infos
        .get(row.saturating_mul(columns).saturating_add(column))
}

/// Fallback font alias every X server provides.
const FALLBACK_TITLE_FONT: &str = "fixed";

/// Loads the configured title font, falling back to [`FALLBACK_TITLE_FONT`]
/// so a missing font cannot prevent the window manager from starting.
fn load_title_font_with_fallback(
    connection: &RustConnection,
    name: &str,
) -> Result<TitleFont, X11Error> {
    match load_title_font(connection, name) {
        Ok(font) => Ok(font),
        Err(error) => {
            if name == FALLBACK_TITLE_FONT {
                return Err(error);
            }
            warn!(font = name, %error, "configured title font unavailable; using fallback");
            load_title_font(connection, FALLBACK_TITLE_FONT)
        }
    }
}

fn load_title_font(connection: &RustConnection, name: &str) -> Result<TitleFont, X11Error> {
    let id = connection.generate_id()?;
    connection.open_font(id, name.as_bytes())?.check()?;
    let query = match connection.query_font(id) {
        Ok(query) => query,
        Err(error) => {
            let _ = connection.close_font(id);
            return Err(error.into());
        }
    };
    let reply = match query.reply() {
        Ok(reply) => reply,
        Err(error) => {
            let _ = connection.close_font(id);
            return Err(error.into());
        }
    };
    Ok(TitleFont {
        id,
        metrics: FontMetrics::from_reply(&reply),
    })
}

fn diagnostic_font_available(connection: &RustConnection, name: &str) -> Result<bool, X11Error> {
    let id = connection.generate_id()?;
    match connection.open_font(id, name.as_bytes())?.check() {
        Ok(()) => {
            connection.close_font(id)?.check()?;
            Ok(true)
        }
        Err(ReplyError::X11Error(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[derive(Clone, Copy)]
struct CursorPalette {
    pointer: Cursor,
    move_window: Cursor,
    top: Cursor,
    bottom: Cursor,
    left: Cursor,
    right: Cursor,
    top_left: Cursor,
    top_right: Cursor,
    bottom_left: Cursor,
    bottom_right: Cursor,
}

impl CursorPalette {
    fn load(connection: &RustConnection) -> Result<Self, X11Error> {
        let font = connection.generate_id()?;
        connection.open_font(font, b"cursor")?.check()?;
        let result = (|| -> Result<Self, X11Error> {
            Ok(Self {
                pointer: create_font_cursor(connection, font, CURSOR_POINTER)?,
                move_window: create_font_cursor(connection, font, CURSOR_MOVE)?,
                top: create_font_cursor(connection, font, CURSOR_TOP_SIDE)?,
                bottom: create_font_cursor(connection, font, CURSOR_BOTTOM_SIDE)?,
                left: create_font_cursor(connection, font, CURSOR_LEFT_SIDE)?,
                right: create_font_cursor(connection, font, CURSOR_RIGHT_SIDE)?,
                top_left: create_font_cursor(connection, font, CURSOR_TOP_LEFT_CORNER)?,
                top_right: create_font_cursor(connection, font, CURSOR_TOP_RIGHT_CORNER)?,
                bottom_left: create_font_cursor(connection, font, CURSOR_BOTTOM_LEFT_CORNER)?,
                bottom_right: create_font_cursor(connection, font, CURSOR_BOTTOM_RIGHT_CORNER)?,
            })
        })();
        let close_result = connection.close_font(font)?.check();
        match (result, close_result) {
            (Ok(cursors), Ok(())) => Ok(cursors),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error.into()),
        }
    }

    const fn for_resize(self, edges: ResizeEdges) -> Cursor {
        match (edges.top, edges.bottom, edges.left, edges.right) {
            (true, _, true, _) => self.top_left,
            (true, _, _, true) => self.top_right,
            (_, true, true, _) => self.bottom_left,
            (_, true, _, true) => self.bottom_right,
            (true, _, _, _) => self.top,
            (_, true, _, _) => self.bottom,
            (_, _, true, _) => self.left,
            (_, _, _, true) => self.right,
            _ => self.bottom_right,
        }
    }

    const fn for_context(self, context: MouseContext) -> Cursor {
        match context {
            MouseContext::Top => self.top,
            MouseContext::Bottom => self.bottom,
            MouseContext::Left => self.left,
            MouseContext::Right => self.right,
            MouseContext::TopLeft => self.top_left,
            MouseContext::TopRight => self.top_right,
            MouseContext::BottomLeft => self.bottom_left,
            MouseContext::BottomRight => self.bottom_right,
            _ => self.pointer,
        }
    }

    const fn as_array(self) -> [Cursor; 10] {
        [
            self.pointer,
            self.move_window,
            self.top,
            self.bottom,
            self.left,
            self.right,
            self.top_left,
            self.top_right,
            self.bottom_left,
            self.bottom_right,
        ]
    }
}

fn create_font_cursor(
    connection: &RustConnection,
    font: Font,
    glyph: u16,
) -> Result<Cursor, X11Error> {
    let cursor = connection.generate_id()?;
    connection
        .create_glyph_cursor(
            cursor,
            font,
            font,
            glyph,
            glyph.saturating_add(1),
            0,
            0,
            0,
            u16::MAX,
            u16::MAX,
            u16::MAX,
        )?
        .check()?;
    Ok(cursor)
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
    button_glyph: u32,
    agent_marker: u32,
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
            theme.button_glyph,
            theme.agent_marker,
        ];
        let mut pixels = [0; 12];
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
            button_glyph: pixels[10],
            agent_marker: pixels[11],
        })
    }

    const fn as_array(self) -> [u32; 12] {
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
            self.button_glyph,
            self.agent_marker,
        ]
    }
}

#[derive(Clone, Copy)]
struct Frame {
    window: Window,
    minimize_button: Option<Window>,
    maximize_button: Option<Window>,
    close_button: Option<Window>,
    resize_handles: ResizeHandles,
    extents: DecorationExtents,
    original_border_width: u16,
}

/// Last decoration-child layout pushed to the server for one frame.
///
/// Captures every input of the client-window, button, and resize-handle
/// configure requests in `configure_decorated_client`; when nothing changed
/// (the common case for every pointer-motion step of a move drag) those
/// requests are provably identical and are skipped.
#[derive(Clone, Copy, Eq, PartialEq)]
struct FrameSyncState {
    frame: Window,
    minimize_button: Option<Window>,
    maximize_button: Option<Window>,
    close_button: Option<Window>,
    extents_left: u32,
    extents_top: u32,
    width: u32,
    height: u32,
    frame_width: u32,
    frame_height: u32,
    handles_enabled: bool,
}

#[derive(Clone, Copy)]
enum FramePart {
    Container(ClientId),
    Button(ClientId, FrameButtonKind),
    ResizeHandle(ClientId, ResizeHandlePart),
}

#[derive(Clone, Copy, Debug)]
struct ResizeHandle {
    window: Window,
    part: ResizeHandlePart,
}

#[derive(Clone, Copy)]
struct ResizeHandles([ResizeHandle; 12]);

impl ResizeHandles {
    fn iter(self) -> impl Iterator<Item = ResizeHandle> {
        self.0.into_iter()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResizeHandlePart {
    Top,
    Bottom,
    Left,
    Right,
    TopLeftHorizontal,
    TopLeftVertical,
    TopRightHorizontal,
    TopRightVertical,
    BottomLeftHorizontal,
    BottomLeftVertical,
    BottomRightHorizontal,
    BottomRightVertical,
}

impl ResizeHandlePart {
    // Corners use perpendicular thin rectangles, as Openbox does, so their
    // useful eight-pixel length never turns into a square over client content.
    const ALL: [Self; 12] = [
        Self::Top,
        Self::Bottom,
        Self::Left,
        Self::Right,
        Self::TopLeftHorizontal,
        Self::TopLeftVertical,
        Self::TopRightHorizontal,
        Self::TopRightVertical,
        Self::BottomLeftHorizontal,
        Self::BottomLeftVertical,
        Self::BottomRightHorizontal,
        Self::BottomRightVertical,
    ];

    const fn context(self) -> MouseContext {
        match self {
            Self::Top => MouseContext::Top,
            Self::Bottom => MouseContext::Bottom,
            Self::Left => MouseContext::Left,
            Self::Right => MouseContext::Right,
            Self::TopLeftHorizontal | Self::TopLeftVertical => MouseContext::TopLeft,
            Self::TopRightHorizontal | Self::TopRightVertical => MouseContext::TopRight,
            Self::BottomLeftHorizontal | Self::BottomLeftVertical => MouseContext::BottomLeft,
            Self::BottomRightHorizontal | Self::BottomRightVertical => MouseContext::BottomRight,
        }
    }
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

#[derive(Clone, Copy, Debug)]
struct InitialGeometry {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    border_width: u16,
}

struct InitialClientMetadata {
    geometry: InitialGeometry,
    initially_iconic: bool,
    urgent: bool,
    normal_hints: NormalHints,
    relationships: Relationships,
    user_time: Option<u32>,
    user_time_window: Option<Window>,
    states: Vec<u32>,
    policy: ClientPolicy,
    application: X11ApplicationIdentity,
    leader: Option<Window>,
    session_id: Option<String>,
    command: Vec<String>,
    desktop: Option<u32>,
    startup_id: Option<String>,
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
struct AbsoluteGeometryRequest {
    x: Option<AxisPosition>,
    y: Option<AxisPosition>,
    width: Option<PositiveRelativeAmount>,
    height: Option<PositiveRelativeAmount>,
    width_basis: SizeBasis,
    height_basis: SizeBasis,
    output: OutputTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlacementOutput {
    Output(Output),
    All,
}

#[derive(Clone, Debug)]
struct EdgeActionField {
    client: Client,
    extents: DecorationExtents,
    geometry: Geometry,
    bounds: Geometry,
    obstacles: Vec<Geometry>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct X11ApplicationIdentity {
    name: String,
    class: String,
    group_name: String,
    group_class: String,
    role: String,
    title: String,
    kind: ApplicationKind,
}

impl X11ApplicationIdentity {
    fn as_application_identity(&self) -> ApplicationIdentity<'_> {
        ApplicationIdentity {
            name: &self.name,
            class: &self.class,
            group_name: &self.group_name,
            group_class: &self.group_class,
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
    kind: FocusCycleKind,
    candidates: Vec<ClientId>,
    index: Option<usize>,
    original: Option<ClientId>,
    modifiers: u16,
    keyboard_grabbed: bool,
}

#[derive(Clone, Copy)]
struct FocusIndicator {
    windows: [Window; 4],
    mapped: bool,
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
    parents: Vec<MenuParent>,
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

struct MenuParent {
    menu: RuntimeMenu,
    overlay: MenuOverlay,
    selected: usize,
    anchor_x: i32,
    anchor_y: i32,
    centered: bool,
}

#[derive(Clone)]
struct RuntimeMenu {
    id: String,
    title: String,
    entries: Vec<RuntimeMenuEntry>,
}

#[derive(Clone)]
enum RuntimeSubmenu {
    Named(String),
    Inline(Box<RuntimeMenu>),
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
        menu: RuntimeSubmenu,
    },
    Separator {
        label: Option<String>,
    },
}

#[derive(Clone)]
enum RuntimeMenuAction {
    Configured(Vec<Action>),
    ActivateClient(ClientId),
    Dismiss,
    SessionLogout,
    Execute(PreparedExecute),
    LaunchApplication(DesktopApplication),
    Exit,
}

#[derive(Clone)]
struct PreparedExecute {
    command: PreparedCommand,
    startup_notify: Option<StartupNotification>,
    target: Option<ClientId>,
    pointer_x: i16,
    pointer_y: i16,
}

#[derive(Clone)]
enum PreparedCommand {
    Shell(String),
    Direct(LaunchCommand),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct StartupSequence {
    name: Option<String>,
    binary: Option<String>,
    wm_class: Option<String>,
    desktop: Option<u32>,
    timestamp: Option<u32>,
    generation: u32,
    initiated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupMessageKind {
    New,
    Change,
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedStartupMessage {
    kind: StartupMessageKind,
    id: String,
    fields: BTreeMap<String, String>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FocusCycleKind {
    Linear,
    Spatial,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FocusRaisePolicy {
    Configured,
    Suppress,
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
enum MaximizeAxis {
    Horizontal,
    Vertical,
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

const fn configured_resize_edges(edge: ResizeEdge) -> ResizeEdges {
    match edge {
        ResizeEdge::Top => ResizeEdges::new(false, false, true, false),
        ResizeEdge::Bottom => ResizeEdges::new(false, false, false, true),
        ResizeEdge::Left => ResizeEdges::new(true, false, false, false),
        ResizeEdge::Right => ResizeEdges::new(false, true, false, false),
        ResizeEdge::TopLeft => ResizeEdges::new(true, false, true, false),
        ResizeEdge::TopRight => ResizeEdges::new(false, true, true, false),
        ResizeEdge::BottomLeft => ResizeEdges::new(true, false, false, true),
        ResizeEdge::BottomRight => ResizeEdges::new(false, true, false, true),
    }
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

fn first_value32(reply: &x11rb::protocol::xproto::GetPropertyReply) -> Option<u32> {
    reply.value32().and_then(|mut values| values.next())
}

fn values32(reply: &x11rb::protocol::xproto::GetPropertyReply) -> Vec<u32> {
    reply
        .value32()
        .map_or_else(Vec::new, |values| values.collect())
}

fn bounded_text_from_reply(reply: &x11rb::protocol::xproto::GetPropertyReply) -> Option<String> {
    (reply.bytes_after == 0 && !reply.value.is_empty() && !reply.value.contains(&0))
        .then(|| x11_text(&reply.value))
}

fn command_from_reply(reply: &x11rb::protocol::xproto::GetPropertyReply) -> Vec<String> {
    if reply.bytes_after != 0 {
        return Vec::new();
    }
    reply
        .value
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .take(64)
        .map(x11_text)
        .collect()
}

fn motif_hints_from_reply(reply: &x11rb::protocol::xproto::GetPropertyReply) -> Option<MotifHints> {
    let mut values = reply.value32()?;
    Some(MotifHints {
        flags: values.next()?,
        functions: values.next()?,
        decorations: values.next()?,
    })
}

fn normal_hints_from_wm(hints: WmSizeHints) -> NormalHints {
    NormalHints {
        size: SizeHints {
            minimum: positive_size(hints.min_size),
            maximum: positive_size(hints.max_size),
            base: nonnegative_size(hints.base_size),
            increment: positive_size(hints.size_increment),
            aspect: aspect_range(hints.aspect),
        },
        gravity: hints.win_gravity.map_or(Gravity::NorthWest, gravity),
        positioned: hints.position.is_some(),
    }
}

fn session_identity_from_parts(
    session_id: Option<String>,
    command: Vec<String>,
    application: &X11ApplicationIdentity,
) -> Option<session::SessionIdentity> {
    if session_id.is_none() && command.is_empty() {
        return None;
    }
    Some(session::SessionIdentity {
        session_id,
        command,
        instance: application.name.clone(),
        class: application.class.clone(),
        role: application.role.clone(),
        kind: application_kind_name(application.kind).to_owned(),
    })
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

fn configured_margin_reservations(config: &Config, screen: Geometry) -> EdgeReservations {
    edge_reservations(
        [
            config.margins.left,
            config.margins.right,
            config.margins.top,
            config.margins.bottom,
        ],
        [
            (0, screen.height.saturating_sub(1)),
            (0, screen.height.saturating_sub(1)),
            (0, screen.width.saturating_sub(1)),
            (0, screen.width.saturating_sub(1)),
        ],
    )
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

fn expand_execute_variables(
    command: &str,
    pid: u32,
    window: Window,
    pointer_x: i16,
    pointer_y: i16,
) -> String {
    let bytes = command.as_bytes();
    let mut expanded = String::with_capacity(command.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'$' {
            let character = command[cursor..]
                .chars()
                .next()
                .expect("cursor remains on a UTF-8 boundary");
            expanded.push(character);
            cursor = cursor.saturating_add(character.len_utf8());
            continue;
        }
        let remaining = &bytes[cursor.saturating_add(1)..];
        let replacement = [
            (b"pointer".as_slice(), format!("{pointer_x} {pointer_y}")),
            (b"pid".as_slice(), pid.to_string()),
            (b"wid".as_slice(), window.to_string()),
        ]
        .into_iter()
        .find(|(name, _)| matches_execute_variable(remaining, name));
        if let Some((name, replacement)) = replacement {
            expanded.push_str(&replacement);
            cursor = cursor.saturating_add(1).saturating_add(name.len());
        } else {
            expanded.push('$');
            cursor = cursor.saturating_add(1);
        }
    }
    expanded
}

fn has_execute_variable(command: &str, name: &[u8]) -> bool {
    let bytes = command.as_bytes();
    bytes.iter().enumerate().any(|(cursor, byte)| {
        *byte == b'$' && matches_execute_variable(&bytes[cursor.saturating_add(1)..], name)
    })
}

fn matches_execute_variable(remaining: &[u8], name: &[u8]) -> bool {
    remaining
        .get(..name.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
        && remaining
            .get(name.len())
            .is_none_or(|next| !next.is_ascii_alphanumeric())
}

fn startup_program(command: &str) -> String {
    let token = command
        .trim_start()
        .split_ascii_whitespace()
        .next()
        .unwrap_or("application")
        .trim_matches(['\'', '"']);
    std::path::Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("application")
        .to_owned()
}

fn startup_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len().saturating_add(2));
    escaped.push('"');
    for character in value.chars() {
        if matches!(character, '"' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('"');
    escaped
}

fn startup_timestamp(id: &str) -> Option<u32> {
    id.rsplit_once("_TIME")?.1.parse().ok()
}

fn parse_startup_message(message: &str) -> Option<ParsedStartupMessage> {
    let (kind, fields) = message.split_once(':')?;
    let kind = match kind {
        "new" => StartupMessageKind::New,
        "change" => StartupMessageKind::Change,
        "remove" => StartupMessageKind::Remove,
        _ => return None,
    };
    let bytes = fields.as_bytes();
    let mut cursor = 0_usize;
    let mut parsed = BTreeMap::new();
    while cursor < bytes.len() {
        while bytes.get(cursor) == Some(&b' ') {
            cursor = cursor.saturating_add(1);
        }
        if cursor == bytes.len() {
            break;
        }
        let key_start = cursor;
        while bytes.get(cursor).is_some_and(|byte| *byte != b'=') {
            if bytes[cursor] == b' ' {
                return None;
            }
            cursor = cursor.saturating_add(1);
        }
        if cursor == bytes.len() || cursor == key_start {
            return None;
        }
        let key = std::str::from_utf8(&bytes[key_start..cursor]).ok()?;
        cursor = cursor.saturating_add(1);
        let mut value = Vec::new();
        let mut quoted = false;
        let mut escaped = false;
        while cursor < bytes.len() {
            let byte = bytes[cursor];
            cursor = cursor.saturating_add(1);
            if escaped {
                value.push(byte);
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = !quoted;
            } else if byte == b' ' && !quoted {
                break;
            } else {
                value.push(byte);
            }
        }
        if escaped || quoted {
            return None;
        }
        parsed.insert(key.to_owned(), String::from_utf8(value).ok()?);
    }
    let id = parsed.get("ID")?.clone();
    if id.is_empty() {
        return None;
    }
    Some(ParsedStartupMessage {
        kind,
        id,
        fields: parsed,
    })
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
        CONTROL_SESSION_SAVE => Some(RuntimeRequest::SessionSave),
        CONTROL_STARTUP_TIMEOUT => Some(RuntimeRequest::StartupTimeout(value)),
        CONTROL_AGENT_TRAFFIC => Some(RuntimeRequest::AgentTraffic),
        CONTROL_AGENT_MARKER => Some(RuntimeRequest::AgentMarkerTimeout),
        CONTROL_AGENT_OBSERVATION => Some(RuntimeRequest::AgentObservationTimeout(value)),
        CONTROL_AGENT_SEMANTIC_READY => Some(RuntimeRequest::AgentSemanticReady(value)),
        CONTROL_AGENT_SEMANTIC_TIMEOUT => Some(RuntimeRequest::AgentSemanticTimeout(value)),
        CONTROL_AGENT_TEXT => Some(RuntimeRequest::AgentText(value)),
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

fn positioned_origin_in_work_area(
    requested: Geometry,
    work_area: Geometry,
    extents: DecorationExtents,
) -> Geometry {
    if requested.x != 0 || requested.y != 0 || (work_area.x == 0 && work_area.y == 0) {
        return requested;
    }
    Geometry::new(
        add_root_offset(work_area.x, extents.left),
        add_root_offset(work_area.y, extents.top),
        requested.width,
        requested.height,
    )
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

fn visible_outer_geometry(client: Client, extents: DecorationExtents) -> Geometry {
    let mut geometry = extents.outer_geometry(client.geometry);
    if client.shaded {
        geometry.height = extents.top.saturating_add(extents.bottom).max(1);
    }
    geometry
}

fn focus_indicator_geometries(outer: Geometry) -> [Geometry; 4] {
    let horizontal_height = FOCUS_INDICATOR_WIDTH.min(outer.height);
    let vertical_width = FOCUS_INDICATOR_WIDTH.min(outer.width);
    let right = geometry_end(outer.x, outer.width)
        .saturating_sub(i32::try_from(vertical_width).unwrap_or(i32::MAX));
    let bottom = geometry_end(outer.y, outer.height)
        .saturating_sub(i32::try_from(horizontal_height).unwrap_or(i32::MAX));
    [
        Geometry::new(outer.x, outer.y, outer.width, horizontal_height),
        Geometry::new(outer.x, outer.y, vertical_width, outer.height),
        Geometry::new(right, outer.y, vertical_width, outer.height),
        Geometry::new(outer.x, bottom, outer.width, horizontal_height),
    ]
}

fn requested_content_dimension(
    amount: Option<PositiveRelativeAmount>,
    basis: SizeBasis,
    reference: u32,
    current: u32,
    decoration: u32,
) -> u32 {
    let Some(amount) = amount else {
        return current;
    };
    let resolved = amount.resolve(reference);
    match basis {
        SizeBasis::Outer => resolved.saturating_sub(decoration).max(1),
        SizeBasis::Content => resolved,
    }
}

fn axis_placement(position: AxisPosition, reference: u32) -> AxisPlacement {
    match position {
        AxisPosition::Start(amount) => AxisPlacement::Start(amount.resolve(reference)),
        AxisPosition::Center => AxisPlacement::Center,
        AxisPosition::End(amount) => AxisPlacement::End(amount.resolve(reference)),
    }
}

fn resolve_output_target(
    outputs: &OutputSet,
    current: Output,
    pointer: Option<Output>,
    target: OutputTarget,
) -> Option<PlacementOutput> {
    let available = outputs.outputs();
    let current_index = available
        .iter()
        .position(|output| output.id == current.id)
        .unwrap_or(0);
    match target {
        OutputTarget::Current => Some(PlacementOutput::Output(current)),
        OutputTarget::Primary => Some(PlacementOutput::Output(outputs.primary())),
        OutputTarget::Pointer => pointer.map(PlacementOutput::Output),
        OutputTarget::Next => Some(PlacementOutput::Output(
            available[(current_index + 1) % available.len()],
        )),
        OutputTarget::Previous => Some(PlacementOutput::Output(
            available[if current_index == 0 {
                available.len() - 1
            } else {
                current_index - 1
            }],
        )),
        OutputTarget::All => Some(PlacementOutput::All),
        OutputTarget::Index(index) => usize::try_from(index.get() - 1)
            .ok()
            .and_then(|index| available.get(index).copied())
            .map(PlacementOutput::Output),
    }
}

const fn spatial_direction(direction: WindowDirection) -> SpatialDirection {
    match direction {
        WindowDirection::Left => SpatialDirection::Left,
        WindowDirection::Right => SpatialDirection::Right,
        WindowDirection::Up => SpatialDirection::Up,
        WindowDirection::Down => SpatialDirection::Down,
        WindowDirection::UpLeft => SpatialDirection::UpLeft,
        WindowDirection::UpRight => SpatialDirection::UpRight,
        WindowDirection::DownLeft => SpatialDirection::DownLeft,
        WindowDirection::DownRight => SpatialDirection::DownRight,
    }
}

const fn cardinal_direction(direction: EdgeDirection) -> CardinalDirection {
    match direction {
        EdgeDirection::Left => CardinalDirection::Left,
        EdgeDirection::Right => CardinalDirection::Right,
        EdgeDirection::Up => CardinalDirection::Up,
        EdgeDirection::Down => CardinalDirection::Down,
    }
}

const fn edge_direction_is_vertical(direction: EdgeDirection) -> bool {
    matches!(direction, EdgeDirection::Up | EdgeDirection::Down)
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

fn frame_button_segments(
    kind: FrameButtonKind,
    size: u32,
    maximized: bool,
    pressed_offset: u32,
) -> ([Segment; 8], usize) {
    let empty = Segment {
        x1: 0,
        y1: 0,
        x2: 0,
        y2: 0,
    };
    let mut segments = [empty; 8];
    let margin = (size / 4).max(2).min(size.saturating_sub(1) / 2);
    let start = margin;
    let end = size.saturating_sub(margin).saturating_sub(1).max(start);
    let coordinate = |value: u32| {
        clamp_i16_u32(
            value
                .saturating_add(pressed_offset)
                .min(size.saturating_sub(1)),
        )
    };
    match kind {
        FrameButtonKind::Minimize => {
            let y = coordinate(end);
            segments[0] = Segment {
                x1: coordinate(start),
                y1: y,
                x2: coordinate(end),
                y2: y,
            };
            (segments, 1)
        }
        FrameButtonKind::Close => {
            segments[0] = Segment {
                x1: coordinate(start),
                y1: coordinate(start),
                x2: coordinate(end),
                y2: coordinate(end),
            };
            segments[1] = Segment {
                x1: coordinate(end),
                y1: coordinate(start),
                x2: coordinate(start),
                y2: coordinate(end),
            };
            (segments, 2)
        }
        FrameButtonKind::Maximize if maximized && end.saturating_sub(start) >= 4 => {
            let inset = 2;
            write_rectangle_segments(
                &mut segments[..4],
                coordinate(start.saturating_add(inset)),
                coordinate(start),
                coordinate(end),
                coordinate(end.saturating_sub(inset)),
            );
            write_rectangle_segments(
                &mut segments[4..],
                coordinate(start),
                coordinate(start.saturating_add(inset)),
                coordinate(end.saturating_sub(inset)),
                coordinate(end),
            );
            (segments, 8)
        }
        FrameButtonKind::Maximize => {
            write_rectangle_segments(
                &mut segments[..4],
                coordinate(start),
                coordinate(start),
                coordinate(end),
                coordinate(end),
            );
            (segments, 4)
        }
    }
}

fn write_rectangle_segments(
    segments: &mut [Segment],
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
) {
    let rectangle = [
        Segment {
            x1: left,
            y1: top,
            x2: right,
            y2: top,
        },
        Segment {
            x1: right,
            y1: top,
            x2: right,
            y2: bottom,
        },
        Segment {
            x1: right,
            y1: bottom,
            x2: left,
            y2: bottom,
        },
        Segment {
            x1: left,
            y1: bottom,
            x2: left,
            y2: top,
        },
    ];
    segments.copy_from_slice(&rectangle);
}

fn title_text_bytes(title: &str, limit: usize) -> Vec<u8> {
    title
        .chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .map(|character| u8::try_from(u32::from(character)).unwrap_or(b'?'))
        .collect()
}

fn fitted_title_text(
    title: &str,
    maximum_width: u32,
    maximum_bytes: usize,
    metrics: &FontMetrics,
) -> (Vec<u8>, u32) {
    let mut width = 0_u32;
    let text = title_text_bytes(title, maximum_bytes)
        .into_iter()
        .take_while(|byte| {
            let next = width.saturating_add(u32::from(metrics.advance(*byte)));
            if next > maximum_width {
                false
            } else {
                width = next;
                true
            }
        })
        .collect();
    (text, width)
}

fn aligned_text_x(alignment: TitleAlignment, left: u32, right: u32, text_width: u32) -> i16 {
    let available = right.saturating_sub(left);
    let remaining = available.saturating_sub(text_width);
    let offset = match alignment {
        TitleAlignment::Left => 0,
        TitleAlignment::Center => remaining / 2,
        TitleAlignment::Right => remaining,
    };
    clamp_i16_u32(left.saturating_add(offset))
}

fn text_baseline(row_y: u32, row_height: u32, metrics: &FontMetrics) -> i16 {
    let font_height = u32::from(metrics.ascent).saturating_add(u32::from(metrics.descent));
    let top = row_height.saturating_sub(font_height) / 2;
    let maximum = row_height.saturating_sub(u32::from(metrics.descent).min(row_height));
    let offset = top.saturating_add(u32::from(metrics.ascent)).min(maximum);
    clamp_i16_u32(row_y.saturating_add(offset))
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

fn resize_handle_geometry(
    part: ResizeHandlePart,
    width: u32,
    height: u32,
    extents: DecorationExtents,
) -> Geometry {
    let grip_width = (width.saturating_sub(1) / 2).clamp(1, RESIZE_HANDLE_SIZE);
    let grip_height = (height.saturating_sub(1) / 2).clamp(1, RESIZE_HANDLE_SIZE);
    let left_depth = extents.left.clamp(1, grip_width);
    let right_depth = extents.right.clamp(1, grip_width);
    let top_depth = extents.top.clamp(1, grip_height);
    let bottom_depth = extents.bottom.clamp(1, grip_height);
    let middle_width = width.saturating_sub(grip_width.saturating_mul(2)).max(1);
    let middle_height = height.saturating_sub(grip_height.saturating_mul(2)).max(1);
    let middle_x = grip_width.min(width.saturating_sub(1));
    let middle_y = grip_height.min(height.saturating_sub(1));
    let right_grip = width.saturating_sub(grip_width);
    let right_edge = width.saturating_sub(right_depth);
    let bottom_grip = height.saturating_sub(grip_height);
    let bottom_edge = height.saturating_sub(bottom_depth);
    let geometry = match part {
        ResizeHandlePart::Top => (middle_x, 0, middle_width, top_depth),
        ResizeHandlePart::Bottom => (middle_x, bottom_edge, middle_width, bottom_depth),
        ResizeHandlePart::Left => (0, middle_y, left_depth, middle_height),
        ResizeHandlePart::Right => (right_edge, middle_y, right_depth, middle_height),
        ResizeHandlePart::TopLeftHorizontal => (0, 0, grip_width, top_depth),
        ResizeHandlePart::TopLeftVertical => (0, 0, left_depth, grip_height),
        ResizeHandlePart::TopRightHorizontal => (right_grip, 0, grip_width, top_depth),
        ResizeHandlePart::TopRightVertical => (right_edge, 0, right_depth, grip_height),
        ResizeHandlePart::BottomLeftHorizontal => (0, bottom_edge, grip_width, bottom_depth),
        ResizeHandlePart::BottomLeftVertical => (0, bottom_grip, left_depth, grip_height),
        ResizeHandlePart::BottomRightHorizontal => {
            (right_grip, bottom_edge, grip_width, bottom_depth)
        }
        ResizeHandlePart::BottomRightVertical => {
            (right_edge, bottom_grip, right_depth, grip_height)
        }
    };
    Geometry::new(
        i32::try_from(geometry.0).unwrap_or(i32::MAX),
        i32::try_from(geometry.1).unwrap_or(i32::MAX),
        geometry.2,
        geometry.3,
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

fn clamp_popup_axis(preferred: i32, origin: i32, extent: u32, child: u32) -> i32 {
    let child = child.min(extent);
    let last =
        geometry_end(origin, extent).saturating_sub(i32::try_from(child).unwrap_or(i32::MAX));
    preferred.clamp(origin, last.max(origin))
}

fn place_submenu_axis(
    parent: i32,
    parent_width: u32,
    origin: i32,
    extent: u32,
    child_width: u32,
) -> i32 {
    let right = geometry_end(parent, parent_width);
    let child = i32::try_from(child_width.min(extent)).unwrap_or(i32::MAX);
    let end = geometry_end(origin, extent);
    let preferred = if right.saturating_add(child) <= end {
        right
    } else {
        parent.saturating_sub(child)
    };
    clamp_popup_axis(preferred, origin, extent, child_width)
}

static COMMAND_MENU_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn create_command_menu_output() -> Result<(PathBuf, File), String> {
    let directory =
        std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    for _ in 0..16 {
        let sequence = COMMAND_MENU_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            "nobox-command-menu-{}-{sequence}.toml",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("could not create bounded output file: {error}")),
        }
    }
    Err("could not allocate a unique command-menu output file".to_owned())
}

fn command_menu_output(command: &str, timeout: Duration) -> Result<String, String> {
    let (path, mut output) = create_command_menu_output()?;
    let result = (|| {
        let child_output = output
            .try_clone()
            .map_err(|error| format!("could not prepare command output: {error}"))?;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::from(child_output))
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start command: {error}"))?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() >= timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("command exceeded {}ms", timeout.as_millis()));
                }
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("could not inspect command: {error}"));
                }
            }
        };
        if !status.success() {
            return Err(format!("command exited with {status}"));
        }
        output
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("could not rewind command output: {error}"))?;
        let mut bytes = Vec::new();
        output
            .by_ref()
            .take(
                u64::try_from(MAX_COMMAND_MENU_BYTES)
                    .expect("the 64 KiB command-menu limit fits in u64")
                    .saturating_add(1),
            )
            .read_to_end(&mut bytes)
            .map_err(|error| format!("could not read command output: {error}"))?;
        if bytes.len() > MAX_COMMAND_MENU_BYTES {
            return Err(format!(
                "command output exceeded {MAX_COMMAND_MENU_BYTES} bytes"
            ));
        }
        String::from_utf8(bytes).map_err(|_| "command output is not valid UTF-8".to_owned())
    })();
    drop(output);
    if let Err(error) = fs::remove_file(&path) {
        warn!(path = %path.display(), %error, "could not remove command-menu output file");
    }
    result
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
                menu: RuntimeSubmenu::Named(menu.clone()),
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

fn runtime_internal_action(label: &str, action: RuntimeMenuAction) -> RuntimeMenuEntry {
    let (label, accelerator) = menu_label(label);
    RuntimeMenuEntry::Item {
        label,
        accelerator,
        action,
        target: None,
    }
}

fn runtime_submenu(label: &str, menu: &str) -> RuntimeMenuEntry {
    let (label, accelerator) = menu_label(label);
    RuntimeMenuEntry::Submenu {
        label,
        accelerator,
        menu: RuntimeSubmenu::Named(menu.to_owned()),
    }
}

fn runtime_inline_submenu(label: &str, menu: RuntimeMenu) -> RuntimeMenuEntry {
    let (label, accelerator) = menu_label(label);
    RuntimeMenuEntry::Submenu {
        label,
        accelerator,
        menu: RuntimeSubmenu::Inline(Box::new(menu)),
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

fn runtime_application(application: DesktopApplication) -> RuntimeMenuEntry {
    RuntimeMenuEntry::Item {
        label: application.name.clone(),
        accelerator: None,
        action: RuntimeMenuAction::LaunchApplication(application),
        target: None,
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

fn menu_row_capacity(output_height: u32, row_height: u32, max_rows: u32) -> usize {
    let available_height = output_height.saturating_sub(20).max(1);
    let fitting_rows = (available_height / row_height.max(1))
        .saturating_sub(1)
        .max(1);
    usize::try_from(max_rows.min(fitting_rows)).unwrap_or(usize::MAX)
}

fn paginate_runtime_menu(mut menu: RuntimeMenu, rows: usize) -> RuntimeMenu {
    if rows < 2 || menu.entries.len() <= rows {
        return menu;
    }

    let page_entries = rows - 1;
    let mut remaining = std::mem::take(&mut menu.entries);
    let mut pages = Vec::new();
    while remaining.len() > page_entries {
        let rest = remaining.split_off(page_entries);
        pages.push(remaining);
        remaining = rest;
    }
    pages.push(remaining);

    let mut pages = pages.into_iter();
    let mut first = pages.next().unwrap_or_default();
    let mut continuation = None;
    for mut entries in pages.rev() {
        if let Some(next) = continuation {
            entries.push(runtime_inline_submenu("_More...", next));
        }
        continuation = Some(RuntimeMenu {
            id: format!("{}:more", menu.id),
            title: "More...".to_owned(),
            entries,
        });
    }
    if let Some(continuation) = continuation {
        first.push(runtime_inline_submenu("_More...", continuation));
    }
    menu.entries = first;
    menu
}

fn menu_frame_entry_at(
    menu: &RuntimeMenu,
    selected: usize,
    overlay: MenuOverlay,
    row_height: u32,
    max_rows: u32,
    root_x: i16,
    root_y: i16,
) -> Option<usize> {
    let x = i32::from(root_x).checked_sub(overlay.x)?;
    let y = i32::from(root_y).checked_sub(overlay.y)?;
    if x < 0
        || y < i32::try_from(row_height).ok()?
        || u32::try_from(x).ok()? >= overlay.width
        || u32::try_from(y).ok()? >= overlay.height
    {
        return None;
    }
    let rows = menu.entries.len().min(
        usize::try_from(
            (overlay.height / row_height)
                .saturating_sub(1)
                .min(max_rows),
        )
        .unwrap_or(usize::MAX),
    );
    let start = focus_cycle_visible_start(menu.entries.len(), selected, rows);
    let row = usize::try_from(u32::try_from(y).ok()? / row_height - 1).ok()?;
    (row < rows).then_some(start + row)
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
        MouseContext::Desktop => &[MouseContext::Desktop, MouseContext::Root],
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

fn canonical_agent_key_name(name: &str) -> &str {
    match name {
        "Enter" => "Return",
        "Esc" => "Escape",
        "PageDown" | "Page_Down" => "Next",
        "PageUp" | "Page_Up" => "Prior",
        "Backspace" => "BackSpace",
        "Space" => "space",
        "ArrowLeft" => "Left",
        "ArrowRight" => "Right",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        _ => name,
    }
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
    deferred_events: &mut VecDeque<Event>,
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
            event => deferred_events.push_back(event),
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
    /// No live nobox supporting window was published by the active manager.
    #[error("no running nobox instance was found on the X11 display")]
    NoRunningManager,
    /// An agent asked for input the manager cannot express.
    #[error("{0}")]
    AgentInput(String),
    /// The live pointer or keyboard destination no longer belongs to the
    /// client named by an otherwise valid agent request.
    #[error("agent input target changed before injection")]
    AgentTargetChanged,
    /// Live X11 hit-test state exceeded the manager's bounded inspection.
    #[error("agent pointer hit-test exceeded its bound")]
    AgentHitTestBound,
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
    /// The child-process reaper could not be started.
    #[error("could not start child-process reaper thread")]
    ProcessReaperThread(#[source] std::io::Error),
    /// The keyboard-chain timeout worker stopped unexpectedly.
    #[error("keyboard-chain timer is unavailable")]
    TimerChannel,
    /// The child-process reaper stopped unexpectedly.
    #[error("child-process reaper is unavailable")]
    ProcessReaperChannel,
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

/// Facts the agent surface needs that only this backend knows.
///
/// Policy decides what a session may see of these; this side only reports
/// what is true.
impl AgentClientDetails for WindowManager {
    fn application(&self, client: ClientId) -> nobox_agent_wire::ApplicationIdentity {
        let Some(identity) = self.application_identities.get(&client) else {
            return nobox_agent_wire::ApplicationIdentity::default();
        };
        nobox_agent_wire::ApplicationIdentity {
            name: non_empty(&identity.name),
            class: non_empty(&identity.class),
            group_name: non_empty(&identity.group_name),
            group_class: non_empty(&identity.group_class),
            role: non_empty(&identity.role),
            kind: agent_application_kind(identity.kind),
        }
    }

    fn title(&self, client: ClientId) -> Option<String> {
        self.titles.get(&client).cloned()
    }

    fn frame(&self, client: ClientId) -> Geometry {
        let content = self
            .clients
            .get(client)
            .map_or_else(|| Geometry::new(0, 0, 1, 1), |managed| managed.geometry);
        self.frames
            .get(&client)
            .map_or(content, |frame| frame.extents.outer_geometry(content))
    }

    fn workspace_name(&self, workspace: WorkspaceId) -> Option<String> {
        self.config
            .workspaces
            .names
            .get(workspace.index() as usize)
            .cloned()
    }

    fn output_name(&self, _output: OutputId) -> Option<String> {
        // RandR output names are not tracked; the protocol allows none.
        None
    }

    fn work_area(&self, output: OutputId) -> Geometry {
        self.output_work_areas
            .get(&(output, self.clients.current_workspace()))
            .copied()
            .unwrap_or(self.root_geometry)
    }
}

/// Converts an empty X11 string to an absent protocol field.
fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

/// Maps a client's functional type onto its protocol name.
const fn agent_application_kind(kind: ApplicationKind) -> nobox_agent_wire::ApplicationKind {
    match kind {
        ApplicationKind::Normal => nobox_agent_wire::ApplicationKind::Normal,
        ApplicationKind::Dialog => nobox_agent_wire::ApplicationKind::Dialog,
        ApplicationKind::Utility => nobox_agent_wire::ApplicationKind::Utility,
        ApplicationKind::Toolbar => nobox_agent_wire::ApplicationKind::Toolbar,
        ApplicationKind::Menu => nobox_agent_wire::ApplicationKind::Menu,
        ApplicationKind::Splash => nobox_agent_wire::ApplicationKind::Splash,
        ApplicationKind::Desktop => nobox_agent_wire::ApplicationKind::Desktop,
        ApplicationKind::Dock => nobox_agent_wire::ApplicationKind::Dock,
        ApplicationKind::DropdownMenu => nobox_agent_wire::ApplicationKind::DropdownMenu,
        ApplicationKind::PopupMenu => nobox_agent_wire::ApplicationKind::PopupMenu,
        ApplicationKind::Tooltip => nobox_agent_wire::ApplicationKind::Tooltip,
        ApplicationKind::Notification => nobox_agent_wire::ApplicationKind::Notification,
        ApplicationKind::Combo => nobox_agent_wire::ApplicationKind::Combo,
        ApplicationKind::DragAndDrop => nobox_agent_wire::ApplicationKind::DragAndDrop,
    }
}

/// Converts a protocol client identity to this backend's.
const fn client_id_from_agent(client: nobox_agent_wire::ClientId) -> ClientId {
    ClientId::new(client.raw())
}

/// Width of the consent dialog.
const AGENT_CONSENT_WIDTH: u16 = 520;

/// Line height inside the consent dialog.
const AGENT_CONSENT_LINE_HEIGHT: u16 = 18;

/// Builds the text of a consent dialog.
///
/// It says what the manager verified, not what the companion claimed, and it
/// describes launching as what it is rather than as picking catalog items.
fn agent_consent_lines(pending: &PendingConsent) -> Vec<String> {
    let mut lines = vec![
        format!("{} wants an agent seat", pending.hello.harness),
        format!("purpose: {}", pending.hello.purpose),
        match pending.executable.as_deref() {
            Some(path) => format!(
                "program: {} (uid {}, pid {})",
                path.display(),
                pending.uid,
                pending.pid
            ),
            None => format!(
                "program: unknown (uid {}, pid {})",
                pending.uid, pending.pid
            ),
        },
    ];
    for bundle in &pending.hello.requested {
        lines.push(format!(
            "  {}: {}",
            bundle.as_str(),
            agent_bundle_summary(*bundle)
        ));
    }
    lines.push("y: allow once    p: allow and remember    n or Escape: deny".to_owned());
    lines
}

/// Describes a capability bundle in the terms it actually grants.
const fn agent_bundle_summary(bundle: nobox_agent_wire::Bundle) -> &'static str {
    match bundle {
        nobox_agent_wire::Bundle::Observe => "see your windows, their titles and positions",
        nobox_agent_wire::Bundle::Accessibility => {
            "read bounded semantic content inside your windows"
        }
        nobox_agent_wire::Bundle::Capture => "see the contents of your windows",
        nobox_agent_wire::Bundle::Input => "type and click in your windows",
        nobox_agent_wire::Bundle::Manage => "move, resize, close and switch your windows",
        nobox_agent_wire::Bundle::Launch => "start approved installed applications",
    }
}

/// Extracts one 8-bit channel from a server pixel.
const fn channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let width = mask.count_ones();
    let value = (pixel & mask) >> shift;
    if width >= 8 {
        (value >> (width - 8)) as u8
    } else {
        ((value << (8 - width)) | (value >> (width.saturating_sub(8 - width)))) as u8
    }
}

/// Returns whether two rectangles share any pixel.
const fn geometries_overlap(left: Geometry, right: Geometry) -> bool {
    let left_right = left.x.saturating_add(left.width as i32);
    let left_bottom = left.y.saturating_add(left.height as i32);
    let right_right = right.x.saturating_add(right.width as i32);
    let right_bottom = right.y.saturating_add(right.height as i32);
    left.x < right_right && right.x < left_right && left.y < right_bottom && right.y < left_bottom
}

/// Returns whether a child-local X11 point lies in one input-shape rectangle.
const fn x11_rectangle_contains(rectangle: Rectangle, x: i16, y: i16) -> bool {
    let left = rectangle.x as i32;
    let top = rectangle.y as i32;
    let right = left + rectangle.width as i32;
    let bottom = top + rectangle.height as i32;
    let x = x as i32;
    let y = y as i32;
    x >= left && x < right && y >= top && y < bottom
}

/// Returns whether every candidate pixel lies inside the enclosing geometry.
fn geometry_contains(enclosing: Geometry, candidate: Geometry) -> bool {
    let enclosing_right = i64::from(enclosing.x) + i64::from(enclosing.width);
    let enclosing_bottom = i64::from(enclosing.y) + i64::from(enclosing.height);
    let candidate_right = i64::from(candidate.x) + i64::from(candidate.width);
    let candidate_bottom = i64::from(candidate.y) + i64::from(candidate.height);
    candidate.x >= enclosing.x
        && candidate.y >= enclosing.y
        && candidate_right <= enclosing_right
        && candidate_bottom <= enclosing_bottom
}

/// Maps a named pointer button onto its X11 button number.
const fn agent_pointer_button(button: nobox_agent_wire::PointerButton) -> u8 {
    match button {
        nobox_agent_wire::PointerButton::Left => 1,
        nobox_agent_wire::PointerButton::Middle => 2,
        nobox_agent_wire::PointerButton::Right => 3,
        nobox_agent_wire::PointerButton::ScrollUp => 4,
        nobox_agent_wire::PointerButton::ScrollDown => 5,
        nobox_agent_wire::PointerButton::ScrollLeft => 6,
        nobox_agent_wire::PointerButton::ScrollRight => 7,
    }
}

/// Maps a character onto the keysym that produces it.
const fn keysym_for_character(character: char) -> Option<u32> {
    let code = character as u32;
    match code {
        0x09 => Some(0xff09),
        0x0a | 0x0d => Some(0xff0d),
        0x08 => Some(0xff08),
        0x1b => Some(0xff1b),
        0x20..=0x7e | 0xa0..=0xff => Some(code),
        // Everything else uses the Unicode keysym range.
        _ => Some(0x0100_0000 + code),
    }
}

/// Plans text against the first two X11 keyboard groups: plain/Shift and
/// AltGr/AltGr+Shift. The complete result is built before callers emit input.
fn plan_agent_text(
    layout: &KeyboardLayout,
    shift_keycode: Option<u8>,
    alt_gr_keycode: Option<u8>,
    text: &str,
) -> Result<Vec<AgentTextStroke>, AgentTextPlanError> {
    let width = usize::from(layout.per_keycode);
    let mut strokes = Vec::new();
    for character in text.chars() {
        let target =
            keysym_for_character(character).ok_or(AgentTextPlanError::Unsupported(character))?;
        let found = (width != 0)
            .then(|| {
                layout
                    .keysyms
                    .chunks(width)
                    .enumerate()
                    .find_map(|(offset, symbols)| {
                        symbols
                            .iter()
                            .take(4)
                            .position(|symbol| *symbol == target)
                            .and_then(|level| {
                                let offset = u8::try_from(offset).ok()?;
                                let keycode = layout.minimum.checked_add(offset)?;
                                Some((keycode, level))
                            })
                    })
            })
            .flatten();
        let Some((keycode, level)) = found else {
            return Err(AgentTextPlanError::Unsupported(character));
        };
        let needs_shift = level % 2 == 1;
        let needs_alt_gr = level >= 2;
        let shift = if needs_shift {
            Some(shift_keycode.ok_or(AgentTextPlanError::MissingModifier {
                character,
                modifier: nobox_agent_wire::Modifier::Shift,
            })?)
        } else {
            None
        };
        let alt_gr = if needs_alt_gr {
            Some(alt_gr_keycode.ok_or(AgentTextPlanError::MissingModifier {
                character,
                modifier: nobox_agent_wire::Modifier::AltGr,
            })?)
        } else {
            None
        };
        strokes.push(AgentTextStroke {
            keycode,
            modifiers: [alt_gr, shift],
        });
    }
    Ok(strokes)
}

/// Converts a client identity to its protocol form.
const fn agent_client_id(client: ClientId) -> nobox_agent_wire::ClientId {
    nobox_agent_wire::ClientId::new(client.raw())
}

/// Intersects a requested crop, given in content coordinates, with the area
/// actually being captured, given in root coordinates.
///
/// Returns `None` when they do not overlap at all: there is no image to send
/// back, and silently substituting the whole window would answer a question
/// nobody asked.
fn clip_capture_rect(
    full: Geometry,
    content_root: (i32, i32),
    requested: nobox_agent_wire::Rect,
) -> Option<Geometry> {
    let requested_left = content_root.0.saturating_add(requested.x);
    let requested_top = content_root.1.saturating_add(requested.y);
    let left = full.x.max(requested_left);
    let top = full.y.max(requested_top);
    let right = full
        .x
        .saturating_add(i32::try_from(full.width).unwrap_or(i32::MAX))
        .min(requested_left.saturating_add(i32::try_from(requested.width).unwrap_or(i32::MAX)));
    let bottom = full
        .y
        .saturating_add(i32::try_from(full.height).unwrap_or(i32::MAX))
        .min(requested_top.saturating_add(i32::try_from(requested.height).unwrap_or(i32::MAX)));
    let width = u32::try_from(right.saturating_sub(left).max(0)).unwrap_or(0);
    let height = u32::try_from(bottom.saturating_sub(top).max(0)).unwrap_or(0);
    if width == 0 || height == 0 {
        return None;
    }
    Some(Geometry {
        x: left,
        y: top,
        width,
        height,
    })
}

fn agent_text_transfer_finish_at(deadline: Instant, last_delivery: Option<Instant>) -> Instant {
    last_delivery
        .map(|delivered| (delivered + AGENT_TEXT_TRANSFER_QUIET).min(deadline))
        .unwrap_or(deadline)
}

const CAPTURE_GRID_LINE: [u8; 3] = [0x00, 0xff, 0xff];
const CAPTURE_GRID_EDGE: [u8; 3] = [0x00, 0x00, 0x00];
const CAPTURE_GRID_LABEL: [u8; 3] = [0xff, 0xff, 0xff];
const CAPTURE_GRID_GLYPH_SCALE: usize = 2;

/// Draws a high-contrast, numerically labelled coordinate grid into RGB
/// capture pixels. Lines are aligned to multiples of `spacing` in the
/// content-coordinate space, not to the image's cropped origin.
fn render_capture_grid(
    rgb: &mut [u8],
    width: usize,
    height: usize,
    spacing: u32,
    origin: (i32, i32),
) {
    let Some(expected) = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
    else {
        return;
    };
    if spacing == 0 || width == 0 || height == 0 || rgb.len() < expected {
        return;
    }

    for_grid_line(origin.0, width, spacing, |x, _| {
        draw_grid_vertical(rgb, width, height, x);
    });
    for_grid_line(origin.1, height, spacing, |y, _| {
        draw_grid_horizontal(rgb, width, height, y);
    });

    // Labels sit on the top and left image edges: their placement conveys the
    // axis, so the raster needs only compact signed decimal digits.
    let mut previous_right = None;
    for_grid_line(origin.0, width, spacing, |x, coordinate| {
        let text = coordinate.to_string();
        let (label_width, _) = grid_label_size(&text);
        let left = x
            .saturating_sub(label_width / 2)
            .min(width.saturating_sub(label_width));
        if previous_right.is_none_or(|right| left > right) {
            draw_grid_label(rgb, width, height, left, 1, &text);
            previous_right = Some(left.saturating_add(label_width).saturating_add(2));
        }
    });

    let mut previous_bottom = None;
    for_grid_line(origin.1, height, spacing, |y, coordinate| {
        let text = coordinate.to_string();
        let (_, label_height) = grid_label_size(&text);
        let top = y
            .saturating_sub(label_height / 2)
            .min(height.saturating_sub(label_height));
        if previous_bottom.is_none_or(|bottom| top > bottom) {
            draw_grid_label(rgb, width, height, 1, top, &text);
            previous_bottom = Some(top.saturating_add(label_height).saturating_add(2));
        }
    });
}

/// Visits every grid line within one image axis, returning both the image
/// pixel and its signed content coordinate.
fn for_grid_line(origin: i32, extent: usize, spacing: u32, mut visit: impl FnMut(usize, i64)) {
    if extent == 0 || spacing == 0 {
        return;
    }
    let origin_wide = i64::from(origin);
    let spacing = i64::from(spacing);
    let extent = i64::try_from(extent.saturating_sub(1)).unwrap_or(i64::MAX);
    let end = origin_wide.saturating_add(extent);
    let mut coordinate = origin_wide.div_euclid(spacing) * spacing;
    if coordinate < origin_wide {
        coordinate = coordinate.saturating_add(spacing);
    }
    while coordinate <= end {
        if let Ok(pixel) = usize::try_from(coordinate - origin_wide) {
            visit(pixel, coordinate);
        }
        coordinate = coordinate.saturating_add(spacing);
        if coordinate == i64::MAX {
            break;
        }
    }
}

fn draw_grid_vertical(rgb: &mut [u8], width: usize, height: usize, x: usize) {
    for y in 0..height {
        if x > 0 {
            set_capture_pixel(rgb, width, x - 1, y, CAPTURE_GRID_EDGE);
        }
        set_capture_pixel(rgb, width, x, y, CAPTURE_GRID_LINE);
        if x + 1 < width {
            set_capture_pixel(rgb, width, x + 1, y, CAPTURE_GRID_EDGE);
        }
    }
}

fn draw_grid_horizontal(rgb: &mut [u8], width: usize, height: usize, y: usize) {
    for x in 0..width {
        if y > 0 {
            set_capture_pixel(rgb, width, x, y - 1, CAPTURE_GRID_EDGE);
        }
        set_capture_pixel(rgb, width, x, y, CAPTURE_GRID_LINE);
        if y + 1 < height {
            set_capture_pixel(rgb, width, x, y + 1, CAPTURE_GRID_EDGE);
        }
    }
}

fn draw_grid_label(
    rgb: &mut [u8],
    width: usize,
    height: usize,
    left: usize,
    top: usize,
    text: &str,
) {
    let (label_width, label_height) = grid_label_size(text);
    fill_capture_rect(
        rgb,
        width,
        height,
        (left, top),
        (label_width, label_height),
        CAPTURE_GRID_EDGE,
    );
    let mut cursor = left.saturating_add(2);
    for character in text.chars() {
        let Some(rows) = capture_grid_glyph(character) else {
            continue;
        };
        for (row, bits) in rows.into_iter().enumerate() {
            for column in 0..3 {
                if bits & (1 << (2 - column)) == 0 {
                    continue;
                }
                fill_capture_rect(
                    rgb,
                    width,
                    height,
                    (
                        cursor + column * CAPTURE_GRID_GLYPH_SCALE,
                        top + 2 + row * CAPTURE_GRID_GLYPH_SCALE,
                    ),
                    (CAPTURE_GRID_GLYPH_SCALE, CAPTURE_GRID_GLYPH_SCALE),
                    CAPTURE_GRID_LABEL,
                );
            }
        }
        cursor = cursor.saturating_add(4 * CAPTURE_GRID_GLYPH_SCALE);
    }
}

fn grid_label_size(text: &str) -> (usize, usize) {
    let characters = text.chars().count();
    let glyphs = characters.saturating_mul(4 * CAPTURE_GRID_GLYPH_SCALE);
    (
        glyphs.saturating_sub(CAPTURE_GRID_GLYPH_SCALE) + 4,
        5 * CAPTURE_GRID_GLYPH_SCALE + 4,
    )
}

const fn capture_grid_glyph(character: char) -> Option<[u8; 5]> {
    match character {
        '0' => Some([0b111, 0b101, 0b101, 0b101, 0b111]),
        '1' => Some([0b010, 0b110, 0b010, 0b010, 0b111]),
        '2' => Some([0b111, 0b001, 0b111, 0b100, 0b111]),
        '3' => Some([0b111, 0b001, 0b111, 0b001, 0b111]),
        '4' => Some([0b101, 0b101, 0b111, 0b001, 0b001]),
        '5' => Some([0b111, 0b100, 0b111, 0b001, 0b111]),
        '6' => Some([0b111, 0b100, 0b111, 0b101, 0b111]),
        '7' => Some([0b111, 0b001, 0b010, 0b010, 0b010]),
        '8' => Some([0b111, 0b101, 0b111, 0b101, 0b111]),
        '9' => Some([0b111, 0b101, 0b111, 0b001, 0b111]),
        '-' => Some([0b000, 0b000, 0b111, 0b000, 0b000]),
        _ => None,
    }
}

fn fill_capture_rect(
    rgb: &mut [u8],
    width: usize,
    height: usize,
    position: (usize, usize),
    size: (usize, usize),
    color: [u8; 3],
) {
    let (left, top) = position;
    let (rect_width, rect_height) = size;
    let right = left.saturating_add(rect_width).min(width);
    let bottom = top.saturating_add(rect_height).min(height);
    for y in top.min(height)..bottom {
        for x in left.min(width)..right {
            set_capture_pixel(rgb, width, x, y, color);
        }
    }
}

fn set_capture_pixel(rgb: &mut [u8], width: usize, x: usize, y: usize, color: [u8; 3]) {
    let Some(offset) = y
        .checked_mul(width)
        .and_then(|row| row.checked_add(x))
        .and_then(|pixel| pixel.checked_mul(3))
    else {
        return;
    };
    let Some(pixel) = rgb.get_mut(offset..offset.saturating_add(3)) else {
        return;
    };
    pixel.copy_from_slice(&color);
}

/// The part of a drawable passed to X11 `GetImage`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DrawableCaptureArea {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

/// Translates a root-stamped capture rectangle into drawable-local X11
/// coordinates without silently truncating values the wire request cannot
/// represent.
fn drawable_capture_area(
    source: Geometry,
    drawable_origin: (i32, i32),
) -> Result<DrawableCaptureArea, X11Error> {
    let x = source.x.checked_sub(drawable_origin.0).ok_or_else(|| {
        X11Error::AgentInput("the capture's horizontal offset overflowed".to_owned())
    })?;
    let y = source.y.checked_sub(drawable_origin.1).ok_or_else(|| {
        X11Error::AgentInput("the capture's vertical offset overflowed".to_owned())
    })?;
    let invalid = || {
        X11Error::AgentInput("the capture area cannot be represented by X11 GetImage".to_owned())
    };
    Ok(DrawableCaptureArea {
        x: i16::try_from(x).map_err(|_| invalid())?,
        y: i16::try_from(y).map_err(|_| invalid())?,
        width: u16::try_from(source.width).map_err(|_| invalid())?,
        height: u16::try_from(source.height).map_err(|_| invalid())?,
    })
}

/// Converts a policy rectangle to its protocol form.
const fn agent_rect(geometry: Geometry) -> nobox_agent_wire::Rect {
    nobox_agent_wire::Rect::new(geometry.x, geometry.y, geometry.width, geometry.height)
}

fn semantic_rect(rect: nobox_agent_wire::Rect) -> Option<semantic::Rect> {
    Some(semantic::Rect {
        x: rect.x,
        y: rect.y,
        width: u16::try_from(rect.width).ok()?,
        height: u16::try_from(rect.height).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_input_rectangles_use_half_open_edges() {
        let rectangle = Rectangle {
            x: -4,
            y: 3,
            width: 10,
            height: 8,
        };
        assert!(x11_rectangle_contains(rectangle, -4, 3));
        assert!(x11_rectangle_contains(rectangle, 5, 10));
        assert!(!x11_rectangle_contains(rectangle, 6, 10));
        assert!(!x11_rectangle_contains(rectangle, 5, 11));
    }

    fn xres_pid_value(client_base: Window, pid: u32) -> ClientIdValue {
        ClientIdValue {
            spec: ClientIdSpec {
                client: client_base,
                mask: ClientIdMask::LOCAL_CLIENT_PID,
            },
            value: vec![pid],
        }
    }

    fn xres_client_value(client_base: Window) -> ClientIdValue {
        ClientIdValue {
            spec: ClientIdSpec {
                client: client_base,
                mask: ClientIdMask::CLIENT_XID,
            },
            value: Vec::new(),
        }
    }

    #[test]
    fn xres_pid_requires_version_1_2_and_one_server_value() {
        let window = 0x40_0001;
        let resource_id_mask = 0x1f_ffff;
        let client_base = window & !resource_id_mask;
        let identity = xres_pid_value(client_base, 1234);
        assert_eq!(
            verified_xres_pid(
                1,
                2,
                window,
                resource_id_mask,
                std::slice::from_ref(&identity)
            ),
            Some(1234)
        );
        assert_eq!(
            verified_xres_pid(
                1,
                1,
                window,
                resource_id_mask,
                std::slice::from_ref(&identity)
            ),
            None
        );
        assert_eq!(
            verified_xres_pid(
                1,
                2,
                window + 1,
                resource_id_mask,
                std::slice::from_ref(&identity)
            ),
            Some(1234)
        );
        assert_eq!(
            verified_xres_pid(
                1,
                2,
                window,
                resource_id_mask,
                &[identity.clone(), identity]
            ),
            None
        );
        assert_eq!(
            verified_xres_pid(
                1,
                2,
                window,
                resource_id_mask,
                &[xres_pid_value(client_base, 0)]
            ),
            None
        );
        assert_eq!(
            verified_xres_pid(
                1,
                2,
                window,
                resource_id_mask,
                &[xres_pid_value(client_base, u32::MAX)]
            ),
            None
        );
        assert_eq!(
            verified_xres_pid(
                1,
                2,
                window,
                resource_id_mask,
                &[xres_pid_value(client_base + resource_id_mask + 1, 1234)]
            ),
            None
        );
    }

    #[test]
    fn xres_client_identity_requires_one_versioned_nonzero_namespace() {
        let identity = xres_client_value(0x40_0000);
        assert_eq!(
            verified_xres_client_base(1, 2, std::slice::from_ref(&identity)),
            Some(0x40_0000)
        );
        assert_eq!(
            verified_xres_client_base(1, 1, std::slice::from_ref(&identity)),
            None
        );
        assert_eq!(
            verified_xres_client_base(1, 2, &[identity.clone(), identity]),
            None
        );
        assert_eq!(
            verified_xres_client_base(1, 2, &[xres_client_value(NONE)]),
            None
        );
        let mut malformed = xres_client_value(0x40_0000);
        malformed.value.push(1);
        assert_eq!(verified_xres_client_base(1, 2, &[malformed]), None);
    }

    fn pending_observation(started: Instant) -> PendingAgentObservation {
        PendingAgentObservation {
            generation: 1,
            session: AgentSessionId::new(1),
            request: AgentRequestId::new(1),
            tool: "client_key",
            action: AgentActionId::new(1),
            target: ClientId::new(1),
            capture: Some(nobox_agent_wire::ObservationCapture::default()),
            committed: vec![AgentStep::Inject],
            started,
            started_sequence: nobox_agent_wire::Sequence::new(4),
            minimum: Duration::from_millis(100),
            quiet: Duration::from_millis(200),
            maximum: Duration::from_millis(500),
            last_event: started,
            events: Vec::new(),
            dropped_events: 0,
        }
    }

    #[test]
    fn action_observation_deadlines_respect_minimum_quiet_and_maximum() {
        let started = Instant::now();
        let mut pending = pending_observation(started);
        assert_eq!(pending.deadline(), started + Duration::from_millis(200));
        pending.last_event = started + Duration::from_millis(50);
        assert_eq!(pending.deadline(), started + Duration::from_millis(250));
        pending.last_event = started + Duration::from_millis(450);
        assert_eq!(pending.deadline(), started + Duration::from_millis(500));
    }

    #[test]
    fn action_observation_capture_may_be_absent_or_follow_a_stable_client() {
        let started = Instant::now();
        let mut pending = pending_observation(started);
        assert_eq!(
            pending.capture_client(),
            Some(nobox_agent_wire::ClientId::new(1))
        );

        pending.capture = None;
        assert_eq!(pending.capture_client(), None);

        pending.capture = Some(nobox_agent_wire::ObservationCapture {
            client: Some(nobox_agent_wire::ClientId::new(2)),
            ..nobox_agent_wire::ObservationCapture::default()
        });
        assert_eq!(
            pending.capture_client(),
            Some(nobox_agent_wire::ClientId::new(2))
        );
    }

    #[test]
    fn action_observation_event_slice_is_bounded() {
        let started = Instant::now();
        let mut pending = pending_observation(started);
        for index in 0..(nobox_agent_wire::MAX_ACTION_OBSERVATION_EVENTS + 3) {
            pending.record(
                nobox_agent_wire::EventEnvelope {
                    sequence: nobox_agent_wire::Sequence::new(index as u64),
                    event: nobox_agent_wire::Event::FocusChanged { client: None },
                },
                started + Duration::from_millis(index as u64),
            );
        }
        assert_eq!(
            pending.events.len(),
            nobox_agent_wire::MAX_ACTION_OBSERVATION_EVENTS
        );
        assert_eq!(pending.dropped_events, 3);
    }

    fn capture_pixel(rgb: &[u8], width: usize, x: usize, y: usize) -> [u8; 3] {
        let offset = (y * width + x) * 3;
        rgb[offset..offset + 3]
            .try_into()
            .expect("one RGB capture pixel")
    }

    #[test]
    fn capture_grid_aligns_to_content_coordinates_not_crop_edges() {
        let width = 140;
        let height = 140;
        let mut rgb = vec![0x80; width * height * 3];

        render_capture_grid(&mut rgb, width, height, 50, (35, 35));

        // Coordinate 50 is image pixel 15, with a cyan center and black
        // contrast edges. The crop edge at pixel zero is not a grid line.
        assert_eq!(capture_pixel(&rgb, width, 15, 30), CAPTURE_GRID_LINE);
        assert_eq!(capture_pixel(&rgb, width, 14, 30), CAPTURE_GRID_EDGE);
        assert_eq!(capture_pixel(&rgb, width, 16, 30), CAPTURE_GRID_EDGE);
        assert_eq!(capture_pixel(&rgb, width, 40, 15), CAPTURE_GRID_LINE);
        assert_eq!(capture_pixel(&rgb, width, 40, 14), CAPTURE_GRID_EDGE);
        assert_eq!(capture_pixel(&rgb, width, 40, 16), CAPTURE_GRID_EDGE);
        assert_eq!(capture_pixel(&rgb, width, 0, 30), [0x80; 3]);

        assert!(
            rgb.chunks_exact(3).any(|pixel| pixel == CAPTURE_GRID_LABEL),
            "numeric labels are rendered above the lines"
        );
    }

    #[test]
    fn capture_grid_handles_negative_origins_and_tiny_images() {
        let width = 80;
        let height = 80;
        let mut rgb = vec![0x80; width * height * 3];
        render_capture_grid(&mut rgb, width, height, 50, (-25, -25));
        assert_eq!(capture_pixel(&rgb, width, 25, 40), CAPTURE_GRID_LINE);
        assert_eq!(capture_pixel(&rgb, width, 40, 25), CAPTURE_GRID_LINE);

        let mut short = vec![0x80; 2];
        render_capture_grid(&mut short, 1, 1, 50, (0, 0));
        assert_eq!(short, vec![0x80; 2]);
    }

    #[test]
    fn execute_context_expansion_matches_openbox_variables() {
        assert!(has_execute_variable("tool $PoInTeR", b"pointer"));
        assert!(has_execute_variable("tool $pid", b"pid"));
        assert!(!has_execute_variable(
            "xscreensaver-command -lock",
            b"pointer"
        ));
        assert!(!has_execute_variable("tool $pid2", b"pid"));
        assert_eq!(
            expand_execute_variables(
                "tool $PID $wid $PoInTeR $unknown $pid2 $wid_",
                42,
                0x1234,
                -12,
                34,
            ),
            "tool 42 4660 -12 34 $unknown $pid2 4660_"
        );
        assert_eq!(
            expand_execute_variables("notify-send '✓ $pid'", 7, NONE, 0, 0),
            "notify-send '✓ 7'"
        );
    }

    #[test]
    fn startup_messages_parse_quoted_and_escaped_fields() {
        let message = parse_startup_message(
            r#"new: ID="nobox_TIME123" NAME="Hello \"world\"" BIN=xterm WMCLASS=XTerm DESKTOP=2"#,
        )
        .expect("valid startup message");
        assert_eq!(message.kind, StartupMessageKind::New);
        assert_eq!(message.id, "nobox_TIME123");
        assert_eq!(
            message.fields.get("NAME").map(String::as_str),
            Some("Hello \"world\"")
        );
        assert_eq!(message.fields.get("DESKTOP").map(String::as_str), Some("2"));
        assert_eq!(startup_timestamp(&message.id), Some(123));
        assert_eq!(startup_value("a \\\" b"), r#""a \\\" b""#);
    }

    #[test]
    fn malformed_startup_messages_are_rejected() {
        for message in [
            "new: NAME=missing-id",
            "new ID=missing-colon",
            "new: ID=unterminated NAME=\"oops",
            "unknown: ID=value",
        ] {
            assert!(parse_startup_message(message).is_none(), "{message}");
        }
    }

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
    fn positioned_origin_moves_inside_left_and_top_work_area_edges() {
        let requested = Geometry::new(0, 0, 800, 600);
        let work_area = Geometry::new(110, 24, 690, 576);
        let extents = DecorationExtents::new(2, 2, 26, 2);

        assert_eq!(
            positioned_origin_in_work_area(requested, work_area, extents),
            Geometry::new(112, 50, 800, 600)
        );
        assert_eq!(
            positioned_origin_in_work_area(Geometry::new(20, 30, 800, 600), work_area, extents,),
            Geometry::new(20, 30, 800, 600)
        );
        assert_eq!(
            positioned_origin_in_work_area(requested, Geometry::new(0, 0, 700, 500), extents,),
            requested
        );
    }

    #[test]
    fn absolute_placement_output_targets_wrap_and_validate_indexes() {
        let first = Output {
            id: OutputId::new(10),
            geometry: Geometry::new(0, 0, 800, 600),
            primary: false,
        };
        let second = Output {
            id: OutputId::new(20),
            geometry: Geometry::new(800, 0, 800, 600),
            primary: true,
        };
        let outputs = OutputSet::new([first, second]);
        assert_eq!(
            resolve_output_target(&outputs, first, None, OutputTarget::Next),
            Some(PlacementOutput::Output(second))
        );
        assert_eq!(
            resolve_output_target(&outputs, first, None, OutputTarget::Previous),
            Some(PlacementOutput::Output(second))
        );
        assert_eq!(
            resolve_output_target(&outputs, first, None, OutputTarget::Primary),
            Some(PlacementOutput::Output(second))
        );
        assert_eq!(
            resolve_output_target(
                &outputs,
                second,
                None,
                OutputTarget::Index(std::num::NonZeroU32::new(1).unwrap()),
            ),
            Some(PlacementOutput::Output(first))
        );
        assert_eq!(
            resolve_output_target(
                &outputs,
                first,
                None,
                OutputTarget::Index(std::num::NonZeroU32::new(3).unwrap()),
            ),
            None
        );
        assert_eq!(
            resolve_output_target(&outputs, first, None, OutputTarget::All),
            Some(PlacementOutput::All)
        );
        assert_eq!(
            resolve_output_target(&outputs, first, Some(second), OutputTarget::Pointer),
            Some(PlacementOutput::Output(second))
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
    fn focus_indicator_follows_outer_edges_and_clamps_to_tiny_frames() {
        assert_eq!(
            focus_indicator_geometries(Geometry::new(-20, 30, 100, 80)),
            [
                Geometry::new(-20, 30, 100, 6),
                Geometry::new(-20, 30, 6, 80),
                Geometry::new(74, 30, 6, 80),
                Geometry::new(-20, 104, 100, 6),
            ]
        );
        assert_eq!(
            focus_indicator_geometries(Geometry::new(4, 5, 3, 2)),
            [
                Geometry::new(4, 5, 3, 2),
                Geometry::new(4, 5, 3, 2),
                Geometry::new(4, 5, 3, 2),
                Geometry::new(4, 5, 3, 2),
            ]
        );
    }

    #[test]
    fn menu_navigation_skips_separators_and_wraps() {
        let configured = [
            MenuEntry::Separator { label: None },
            MenuEntry::Item {
                label: "_one".to_owned(),
                actions: vec![Action::Exit { prompt: true }],
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
        assert_eq!(place_submenu_axis(500, 260, 0, 800, 260), 240);
        assert_eq!(place_submenu_axis(10, 260, 0, 800, 260), 270);
        assert_eq!(clamp_popup_axis(554, 0, 600, 52), 548);

        let menu = RuntimeMenu {
            id: "root".to_owned(),
            title: "Root".to_owned(),
            entries,
        };
        let overlay = MenuOverlay {
            window: 1,
            x: 500,
            y: 502,
            width: 260,
            height: 78,
            mapped: true,
        };
        assert_eq!(
            menu_frame_entry_at(&menu, 1, overlay, 26, 8, 510, 527),
            None
        );
        assert_eq!(
            menu_frame_entry_at(&menu, 1, overlay, 26, 8, 510, 528),
            Some(0)
        );
        assert_eq!(
            menu_frame_entry_at(&menu, 1, overlay, 26, 8, 510, 554),
            Some(1)
        );
        assert_eq!(
            menu_frame_entry_at(&menu, 1, overlay, 26, 8, 760, 554),
            None
        );
    }

    #[test]
    fn menu_overflow_uses_more_submenus_instead_of_scrolling() {
        let entries = (0..8)
            .map(|index| {
                runtime_internal_action(&format!("Item {index}"), RuntimeMenuAction::Dismiss)
            })
            .collect();
        let menu = paginate_runtime_menu(
            RuntimeMenu {
                id: "long".to_owned(),
                title: "Long".to_owned(),
                entries,
            },
            4,
        );

        let mut page = &menu;
        let mut page_lengths = Vec::new();
        loop {
            page_lengths.push(page.entries.len());
            let Some(RuntimeMenuEntry::Submenu {
                label,
                menu: RuntimeSubmenu::Inline(next),
                ..
            }) = page.entries.last()
            else {
                break;
            };
            assert_eq!(label, "More...");
            page = next;
        }

        assert_eq!(page_lengths, vec![4, 4, 2]);
        assert_eq!(menu_row_capacity(600, 26, 20), 20);
        assert_eq!(menu_row_capacity(100, 26, 20), 2);
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
    fn resize_handles_stay_within_decorations() {
        let extents = DecorationExtents::new(2, 2, 26, 2);
        assert_eq!(
            resize_handle_geometry(ResizeHandlePart::Left, 204, 148, extents),
            Geometry::new(0, 8, 2, 132)
        );
        assert_eq!(
            resize_handle_geometry(ResizeHandlePart::TopLeftHorizontal, 204, 148, extents,),
            Geometry::new(0, 0, 8, 8)
        );
        assert_eq!(
            resize_handle_geometry(ResizeHandlePart::BottomRightHorizontal, 204, 148, extents,),
            Geometry::new(196, 146, 8, 2)
        );
        assert_eq!(
            resize_handle_geometry(ResizeHandlePart::BottomRightVertical, 204, 148, extents,),
            Geometry::new(202, 140, 2, 8)
        );

        let content = Geometry::new(2, 26, 200, 120);
        let content_right = geometry_end(content.x, content.width);
        let content_bottom = geometry_end(content.y, content.height);
        for part in ResizeHandlePart::ALL {
            let handle = resize_handle_geometry(part, 204, 148, extents);
            let handle_right = geometry_end(handle.x, handle.width);
            let handle_bottom = geometry_end(handle.y, handle.height);
            assert!(
                handle_right <= content.x
                    || handle.x >= content_right
                    || handle_bottom <= content.y
                    || handle.y >= content_bottom,
                "{part:?} overlaps client content: {handle:?}"
            );
        }
    }

    #[test]
    fn resize_handles_remain_valid_for_tiny_frames() {
        let extents = DecorationExtents::new(2, 2, 2, 2);
        for part in ResizeHandlePart::ALL {
            let geometry = resize_handle_geometry(part, 3, 3, extents);
            assert!(geometry.width > 0, "{part:?} has zero width");
            assert!(geometry.height > 0, "{part:?} has zero height");
            assert!(geometry.x >= 0, "{part:?} starts left of the frame");
            assert!(geometry.y >= 0, "{part:?} starts above the frame");
            assert!(
                u32::try_from(geometry.x).unwrap_or(u32::MAX) < 3,
                "{part:?} starts right of the frame"
            );
            assert!(
                u32::try_from(geometry.y).unwrap_or(u32::MAX) < 3,
                "{part:?} starts below the frame"
            );
            assert!(
                u32::try_from(geometry.x)
                    .unwrap_or(u32::MAX)
                    .saturating_add(geometry.width)
                    <= 3,
                "{part:?} extends right of the frame"
            );
            assert!(
                u32::try_from(geometry.y)
                    .unwrap_or(u32::MAX)
                    .saturating_add(geometry.height)
                    <= 3,
                "{part:?} extends below the frame"
            );
        }
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
    fn a_capture_crop_is_clipped_into_the_window_it_came_from() {
        let full = Geometry {
            x: 100,
            y: 50,
            width: 800,
            height: 600,
        };
        // An ordinary crop, in content coordinates, lands at the right place
        // in root coordinates.
        let inside = clip_capture_rect(
            full,
            (full.x, full.y),
            nobox_agent_wire::Rect::new(10, 20, 200, 100),
        )
        .expect("overlaps");
        assert_eq!((inside.x, inside.y), (110, 70));
        assert_eq!((inside.width, inside.height), (200, 100));

        // One running off the edge is trimmed rather than refused: a request
        // near a border is still a useful request.
        let clipped = clip_capture_rect(
            full,
            (full.x, full.y),
            nobox_agent_wire::Rect::new(700, 0, 400, 50),
        )
        .expect("overlaps");
        assert_eq!(clipped.x, 800);
        assert_eq!(clipped.width, 100, "trimmed to the window's right edge");

        // One that misses entirely has no answer, and quietly substituting the
        // whole window would answer a question nobody asked.
        assert!(
            clip_capture_rect(
                full,
                (full.x, full.y),
                nobox_agent_wire::Rect::new(900, 0, 100, 100),
            )
            .is_none()
        );
        assert!(
            clip_capture_rect(
                full,
                (full.x, full.y),
                nobox_agent_wire::Rect::new(0, 600, 100, 100),
            )
            .is_none()
        );

        // A frame extends into negative content coordinates. A crop naming
        // those coordinates reaches the titlebar/border rather than being
        // silently reinterpreted as frame-local coordinates.
        let frame = Geometry::new(95, 20, 810, 635);
        let titlebar = clip_capture_rect(
            frame,
            (100, 50),
            nobox_agent_wire::Rect::new(-5, -30, 10, 30),
        )
        .expect("frame crop overlaps");
        assert_eq!(titlebar, Geometry::new(95, 20, 10, 30));
    }

    #[test]
    fn exact_text_waits_for_quiet_but_never_extends_its_absolute_deadline() {
        let started = Instant::now();
        let deadline = started + AGENT_TEXT_TRANSFER_TIMEOUT;
        assert_eq!(agent_text_transfer_finish_at(deadline, None), deadline);
        assert_eq!(
            agent_text_transfer_finish_at(deadline, Some(started + Duration::from_millis(100))),
            started + Duration::from_millis(350)
        );
        assert_eq!(
            agent_text_transfer_finish_at(deadline, Some(deadline - Duration::from_millis(10))),
            deadline
        );
    }

    #[test]
    fn a_capture_crop_is_read_from_its_drawable_local_origin() {
        let source = Geometry {
            x: 180,
            y: 90,
            width: 200,
            height: 100,
        };

        let area = drawable_capture_area(source, (100, 50)).expect("representable crop");

        assert_eq!(
            area,
            DrawableCaptureArea {
                x: 80,
                y: 40,
                width: 200,
                height: 100,
            }
        );
    }

    #[test]
    fn a_capture_crop_rejects_coordinates_x11_cannot_represent() {
        let source = Geometry {
            x: 40_000,
            y: 0,
            width: 10,
            height: 10,
        };

        let result = drawable_capture_area(source, (0, 0));

        assert!(matches!(result, Err(X11Error::AgentInput(_))));
    }

    #[test]
    fn off_screen_capture_geometry_requires_indirect_pixels() {
        let output = Geometry::new(0, 0, 1280, 800);

        assert!(geometry_contains(output, Geometry::new(80, 60, 1100, 700)));
        assert!(!geometry_contains(output, Geometry::new(80, 60, 1100, 743)));
        assert!(!geometry_contains(output, Geometry::new(-1, 0, 100, 100)));
    }

    #[test]
    fn keycode_lookup_checks_every_keyboard_column() {
        let mapping = [xkeysym::key::a, xkeysym::key::A, 0, xkeysym::key::Return];
        assert_eq!(keycodes_for_named_symbol(8, 2, &mapping, "A"), [8]);
        assert_eq!(keycodes_for_named_symbol(8, 2, &mapping, "Return"), [9]);
    }

    #[test]
    fn agent_key_names_accept_common_cross_tool_aliases() {
        for (alias, canonical) in [
            ("Enter", "Return"),
            ("Esc", "Escape"),
            ("PageDown", "Next"),
            ("Page_Down", "Next"),
            ("PageUp", "Prior"),
            ("Page_Up", "Prior"),
            ("Backspace", "BackSpace"),
            ("Space", "space"),
            ("ArrowLeft", "Left"),
            ("ArrowRight", "Right"),
            ("ArrowUp", "Up"),
            ("ArrowDown", "Down"),
        ] {
            assert_eq!(canonical_agent_key_name(alias), canonical);
        }
        assert_eq!(canonical_agent_key_name("XF86AudioPlay"), "XF86AudioPlay");

        let paging = [xkeysym::key::Prior, xkeysym::key::Next];
        assert_eq!(
            keycodes_for_named_symbol(8, 1, &paging, canonical_agent_key_name("PageUp")),
            [8]
        );
        assert_eq!(
            keycodes_for_named_symbol(8, 1, &paging, canonical_agent_key_name("PageDown")),
            [9]
        );
    }

    #[test]
    fn agent_text_plans_plain_shift_and_alt_gr_levels() {
        let layout = KeyboardLayout {
            minimum: 10,
            per_keycode: 4,
            keysyms: vec![
                xkeysym::key::_2,
                xkeysym::key::quotedbl,
                xkeysym::key::at,
                xkeysym::key::sterling,
            ],
        };

        let strokes = plan_agent_text(&layout, Some(50), Some(108), "2\"@£").expect("typable");

        assert_eq!(
            strokes,
            [
                AgentTextStroke {
                    keycode: 10,
                    modifiers: [None, None],
                },
                AgentTextStroke {
                    keycode: 10,
                    modifiers: [None, Some(50)],
                },
                AgentTextStroke {
                    keycode: 10,
                    modifiers: [Some(108), None],
                },
                AgentTextStroke {
                    keycode: 10,
                    modifiers: [Some(108), Some(50)],
                },
            ]
        );
    }

    #[test]
    fn agent_text_rejects_an_unsupported_suffix_before_returning_a_plan() {
        let layout = KeyboardLayout {
            minimum: 8,
            per_keycode: 2,
            keysyms: vec![xkeysym::key::a, xkeysym::key::A],
        };

        assert_eq!(
            plan_agent_text(&layout, Some(50), Some(108), "a@"),
            Err(AgentTextPlanError::Unsupported('@'))
        );
    }

    #[test]
    fn agent_text_refuses_a_level_without_its_modifier_key() {
        let layout = KeyboardLayout {
            minimum: 11,
            per_keycode: 4,
            keysyms: vec![
                xkeysym::key::_2,
                xkeysym::key::quotedbl,
                xkeysym::key::at,
                0,
            ],
        };

        assert_eq!(
            plan_agent_text(&layout, Some(50), None, "@"),
            Err(AgentTextPlanError::MissingModifier {
                character: '@',
                modifier: nobox_agent_wire::Modifier::AltGr,
            })
        );
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
    fn title_text_uses_font_advances_for_clipping_and_alignment() {
        let mut advances = [8; 256];
        advances[usize::from(b'W')] = 10;
        advances[usize::from(b'i')] = 3;
        let metrics = FontMetrics {
            advances,
            ascent: 9,
            descent: 3,
        };
        assert_eq!(
            fitted_title_text("Wii", 13, 255, &metrics),
            (b"Wi".to_vec(), 13)
        );
        assert_eq!(
            fitted_title_text("Wii", 12, 255, &metrics),
            (b"W".to_vec(), 10)
        );
        assert_eq!(aligned_text_x(TitleAlignment::Left, 6, 106, 20), 6);
        assert_eq!(aligned_text_x(TitleAlignment::Center, 6, 106, 20), 46);
        assert_eq!(aligned_text_x(TitleAlignment::Right, 6, 106, 20), 86);
        assert_eq!(text_baseline(20, 24, &metrics), 35);
    }

    #[test]
    fn frame_buttons_are_laid_out_from_the_right_edge() {
        assert_eq!(button_x(400, 16, 0), 380);
        assert_eq!(button_x(400, 16, 1), 360);
    }

    #[test]
    fn frame_button_glyphs_are_bounded_and_reflect_runtime_state() {
        let (close, close_count) = frame_button_segments(FrameButtonKind::Close, 16, false, 0);
        assert_eq!(close_count, 2);
        assert_eq!((close[0].x1, close[0].y1), (4, 4));
        assert_eq!((close[0].x2, close[0].y2), (11, 11));

        let (pressed, pressed_count) =
            frame_button_segments(FrameButtonKind::Minimize, 16, false, 1);
        assert_eq!(pressed_count, 1);
        assert_eq!((pressed[0].x1, pressed[0].y1, pressed[0].x2), (5, 12, 12));

        let (_, maximize_count) = frame_button_segments(FrameButtonKind::Maximize, 16, false, 0);
        let (restore, restore_count) =
            frame_button_segments(FrameButtonKind::Maximize, 16, true, 0);
        assert_eq!(maximize_count, 4);
        assert_eq!(restore_count, 8);
        assert_eq!((restore[0].x1, restore[0].y1), (6, 4));

        let (tiny, tiny_count) = frame_button_segments(FrameButtonKind::Close, 1, false, 1);
        assert_eq!(tiny_count, 2);
        assert_eq!(
            (tiny[0].x1, tiny[0].y1, tiny[0].x2, tiny[0].y2),
            (0, 0, 0, 0)
        );
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
            runtime_request(CONTROL_SESSION_SAVE, 0, 0),
            Some(RuntimeRequest::SessionSave)
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
        assert_eq!(
            runtime_request(CONTROL_STARTUP_TIMEOUT, 12, 0),
            Some(RuntimeRequest::StartupTimeout(12))
        );
        assert_eq!(
            runtime_request(CONTROL_AGENT_OBSERVATION, 17, 0),
            Some(RuntimeRequest::AgentObservationTimeout(17))
        );
        assert_eq!(
            runtime_request(CONTROL_AGENT_SEMANTIC_READY, 18, 0),
            Some(RuntimeRequest::AgentSemanticReady(18))
        );
        assert_eq!(
            runtime_request(CONTROL_AGENT_SEMANTIC_TIMEOUT, 19, 0),
            Some(RuntimeRequest::AgentSemanticTimeout(19))
        );
        assert_eq!(
            runtime_request(CONTROL_AGENT_TEXT, 20, 0),
            Some(RuntimeRequest::AgentText(20))
        );
        assert_eq!(runtime_request(0, 0, 0), None);
        assert_eq!(runtime_request(u32::MAX, 0, 0), None);
    }

    #[test]
    fn semantic_deadline_precedes_a_later_agent_marker() {
        let marker = Instant::now() + Duration::from_secs(2);
        let observations = BTreeMap::new();
        let semantic_deadline = marker - Duration::from_secs(1);
        let semantics = BTreeMap::from([(7, semantic_deadline)]);
        let pings = BTreeMap::new();
        let startups = BTreeMap::new();

        let deadline = next_runtime_deadline(
            None,
            [Some(marker), None],
            &observations,
            &semantics,
            &pings,
            None,
            &startups,
        );

        assert_eq!(deadline, Some(semantic_deadline));
    }

    #[test]
    fn semantic_tree_remaps_ids_and_bounds_continuations() {
        let generation = nobox_agent_wire::TreeGeneration::new(3);
        let mut tree = AgentSemanticTree::new(generation, 700);
        assert_eq!(
            tree.public_id(700),
            nobox_agent_wire::SemanticNodeId::new(1)
        );
        assert_eq!(
            tree.public_id(800),
            nobox_agent_wire::SemanticNodeId::new(2)
        );
        assert_eq!(
            tree.public_id(800),
            nobox_agent_wire::SemanticNodeId::new(2)
        );

        for offset in 1..=MAX_SEMANTIC_CONTINUATIONS + 1 {
            tree.issue_continuation(AgentSemanticCursor::Tree {
                root: 700,
                offset: u16::try_from(offset).expect("small offset"),
                max_depth: 4,
            });
        }
        assert_eq!(tree.continuations.len(), MAX_SEMANTIC_CONTINUATIONS);
        assert!(
            !tree
                .continuations
                .contains_key(&nobox_agent_wire::SemanticContinuation::new(1))
        );
        assert!(
            tree.continuations
                .contains_key(&nobox_agent_wire::SemanticContinuation::new(2))
        );
    }

    #[test]
    fn semantic_projection_requires_exact_breadth_first_pages() {
        let projection = PendingSemanticProjection {
            tree_generation: nobox_agent_wire::TreeGeneration::FIRST,
            root: 7,
            offset: 0,
            max_nodes: 2,
            max_depth: 2,
            source_continuation: None,
        };
        let root = semantic::Root {
            id: 7,
            role: nobox_agent_wire::SemanticRole::Window,
            name: None,
            states: Vec::new(),
            bounds: nobox_agent_wire::Rect::new(0, 0, 100, 100),
            child_count: 1,
        };
        let root_node = semantic::Node {
            id: 7,
            parent: None,
            depth: 0,
            role: nobox_agent_wire::SemanticRole::Window,
            name: None,
            states: Vec::new(),
            bounds: Some(nobox_agent_wire::Rect::new(0, 0, 100, 100)),
            child_count: 1,
        };
        let child = semantic::Node {
            id: 8,
            parent: Some(7),
            depth: 1,
            role: nobox_agent_wire::SemanticRole::Button,
            name: Some("Continue".to_owned()),
            states: Vec::new(),
            bounds: None,
            child_count: 0,
        };
        let page = semantic::Match {
            root: root.clone(),
            nodes: vec![root_node.clone(), child.clone()],
            next_offset: Some(2),
        };
        assert!(valid_semantic_projection(projection, &page));

        let malformed = [
            semantic::Match {
                root: root.clone(),
                nodes: vec![child.clone()],
                next_offset: None,
            },
            semantic::Match {
                root: root.clone(),
                nodes: vec![root_node.clone(), child.clone()],
                next_offset: Some(1),
            },
            semantic::Match {
                root,
                nodes: vec![root_node, semantic::Node { depth: 2, ..child }],
                next_offset: None,
            },
        ];
        for page in malformed {
            assert!(!valid_semantic_projection(projection, &page));
        }
    }

    #[test]
    fn semantic_search_rechecks_predicates_and_cursor_progress() {
        let search = PendingSemanticSearch {
            tree_generation: nobox_agent_wire::TreeGeneration::FIRST,
            offset: 3,
            max_results: 1,
            query: nobox_agent_wire::SemanticQuery {
                name: Some("continue".to_owned()),
                roles: vec![nobox_agent_wire::SemanticRole::Button],
                states: vec![nobox_agent_wire::SemanticState::Visible],
            },
            source_continuation: None,
        };
        let root = semantic::Root {
            id: 7,
            role: nobox_agent_wire::SemanticRole::Window,
            name: None,
            states: Vec::new(),
            bounds: nobox_agent_wire::Rect::new(0, 0, 100, 100),
            child_count: 1,
        };
        let matched = semantic::Node {
            id: 8,
            parent: Some(7),
            depth: 1,
            role: nobox_agent_wire::SemanticRole::Button,
            name: Some("Continue setup".to_owned()),
            states: vec![nobox_agent_wire::SemanticState::Visible],
            bounds: None,
            child_count: 0,
        };
        let page = semantic::Match {
            root: root.clone(),
            nodes: vec![matched.clone()],
            next_offset: Some(9),
        };
        assert!(valid_semantic_search(&search, &page));

        for page in [
            semantic::Match {
                root: root.clone(),
                nodes: vec![semantic::Node {
                    role: nobox_agent_wire::SemanticRole::Link,
                    ..matched.clone()
                }],
                next_offset: None,
            },
            semantic::Match {
                root: root.clone(),
                nodes: vec![matched.clone()],
                next_offset: Some(3),
            },
            semantic::Match {
                root,
                nodes: Vec::new(),
                next_offset: Some(9),
            },
        ] {
            assert!(!valid_semantic_search(&search, &page));
        }
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
            insert_key_binding_variants(
                &mut tree,
                &[vec![(10, 1)]],
                &[Action::Exit { prompt: true }],
            ),
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
