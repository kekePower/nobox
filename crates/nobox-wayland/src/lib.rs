//! Native Wayland compositor backend, currently available as a managed nested shell.
//!
//! The backend owns Wayland protocol translation and rendering while window
//! management decisions remain in `nobox-core`.

mod direct;
mod direct_runtime;
mod menu;
mod text;

pub use direct::{
    DirectConnector, DirectDeviceDiagnostics, DirectDiagnostics, DirectDiagnosticsError,
    DirectMode, DirectOutputState, DirectTopology, DirectTopologyError,
};
pub use direct_runtime::{DirectOptions, DirectRunReport, run_direct_with_session};

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    env,
    ffi::OsString,
    fs,
    os::{
        fd::{AsFd as _, OwnedFd},
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use menu::{
    MenuLevel, MenuSession, RuntimeMenu, RuntimeMenuAction, RuntimeMenuEntry, RuntimeSubmenu,
    action_entry, configured_entry, paginate_runtime_menu, submenu_entry,
};
use nobox_config::{
    Action, ActionQuery, ActionQueryContext, ActionQueryTarget, ApplicationIdentity,
    ApplicationKind, ApplicationLayer, ApplicationWorkspace, AxisPosition, Config, EdgeDirection,
    KeyChord, KeyboardModifier, LayerTarget, MAX_COMMAND_MENU_BYTES, MarginConfig,
    MaximizeDirection, MenuDefinition, MenuSource, MouseContext, MouseTrigger, OutputTarget,
    PositiveRelativeAmount, ResizeEdge, ScreenshotTarget, SizeBasis, TitleAlignment,
    WindowDirection, WorkspacePlacement, mouse_context_chain,
};
use nobox_core::{
    AxisPlacement, BlockingEdgePolicy, CardinalDirection, Client as PolicyClient,
    ClientId as PolicyClientId, ClientLayer, ClientPolicy, ClientPresentation, ClientRole,
    ClientSet, DecorationExtents, DecorationOverride, EdgeReservation, EdgeReservations, Geometry,
    Gravity, Output as PolicyOutput, OutputId, OutputSet, ResizeDeltas, ResizeEdges, Size,
    SizeHints, SpatialDirection, TransientTarget, WorkspaceAssignment, WorkspaceCorner,
    WorkspaceDirection, WorkspaceId, WorkspaceLayout, WorkspaceOrientation,
    directional_grow_geometry, directional_move_geometry, directional_shrink_geometry,
    directional_target, grow_to_fill_geometry, keyboard_move_geometry, move_resize_geometry,
    pointer_resize_geometry, relative_resize_geometry, smart_placement,
};
use nobox_desktop::{ApplicationCatalog, DesktopApplication};
use nobox_runtime::session::{
    SessionClient, SessionDecorationOverride, SessionIdentity, SessionLayer,
};
use nobox_runtime::{
    BackendKind, ControlRequest, ControlSender, ControlServer, RunDisposition, SessionRestore,
    SessionSnapshot, bounded_shell_output,
};
use smithay::{
    backend::{
        allocator::{Fourcc, dmabuf::Dmabuf},
        input::{
            AbsolutePositionEvent as _, Axis, ButtonState, Event as _, InputEvent, KeyState,
            KeyboardKeyEvent as _, PointerAxisEvent as _, PointerButtonEvent as _, TouchEvent as _,
        },
        renderer::{
            Bind as _, Color32F, ExportMem as _, Frame as _, Offscreen as _, Renderer as _,
            element::{
                Kind,
                solid::{SolidColorBuffer, SolidColorRenderElement},
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
            pixman::PixmanRenderer,
            utils::{draw_render_elements, on_commit_buffer_handler, with_renderer_surface_state},
        },
        winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend},
    },
    delegate_compositor, delegate_dmabuf, delegate_drm_syncobj, delegate_foreign_toplevel_list,
    delegate_fractional_scale, delegate_layer_shell, delegate_output, delegate_shm,
    delegate_viewporter, delegate_xdg_activation, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        LayerSurface as DesktopLayerSurface, PopupKeyboardGrab, PopupManager, PopupPointerGrab,
        Space, Window, WindowSurfaceType, find_popup_root_surface, layer_map_for_output,
    },
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, Keycode, KeysymHandle, ModifiersState, xkb},
        pointer::{
            AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, CursorImageSurfaceData, Focus,
            GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
            GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
            GestureSwipeEndEvent, GestureSwipeUpdateEvent, MotionEvent, RelativeMotionEvent,
        },
        touch::{
            DownEvent as TouchDownEvent, MotionEvent as TouchMotionEvent, UpEvent as TouchUpEvent,
        },
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            EventLoop, Interest, Mode, PostAction,
            channel::{self, Event as ChannelEvent},
            generic::Generic,
        },
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel,
        },
        wayland_server::{
            Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource as _,
            backend::{ClientData, ClientId, DisconnectReason, GlobalId, ObjectId},
            protocol::{
                wl_buffer, wl_data_device, wl_data_device::WlDataDevice, wl_data_device_manager,
                wl_data_device_manager::WlDataDeviceManager, wl_data_source,
                wl_data_source::WlDataSource, wl_keyboard::WlKeyboard, wl_output::WlOutput,
                wl_pointer::WlPointer, wl_seat, wl_seat::WlSeat, wl_surface::WlSurface,
                wl_touch::WlTouch,
            },
        },
    },
    utils::{Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Serial, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            Blocker, BlockerState, CompositorClientState, CompositorHandler, CompositorState,
            SurfaceAttributes, TraversalAction, add_blocker, add_pre_commit_hook, with_states,
            with_surface_tree_downward,
        },
        cursor_shape::{CursorShapeDeviceUserData, CursorShapeManagerState},
        dmabuf::{DmabufFeedback, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        drm_syncobj::{
            DrmSyncPointSource, DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState,
        },
        foreign_toplevel_list::{
            ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
        },
        fractional_scale::{
            FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale,
        },
        keyboard_shortcuts_inhibit::{
            KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState,
            KeyboardShortcutsInhibitor, KeyboardShortcutsInhibitorSeat,
            KeyboardShortcutsInhibitorUserData,
        },
        output::{OutputHandler, OutputManagerState},
        pointer_constraints::{
            PointerConstraint, PointerConstraintUserData, PointerConstraintsHandler,
            PointerConstraintsState, with_pointer_constraint,
        },
        pointer_gestures::{PointerGestureUserData, PointerGesturesState},
        presentation::{
            PresentationFeedbackCachedState, PresentationFeedbackState, PresentationState, Refresh,
        },
        relative_pointer::{RelativePointerManagerState, RelativePointerUserData},
        seat::{
            KeyboardUserData, PointerUserData, SeatGlobalData, SeatUserData, TouchUserData,
            WaylandFocus,
        },
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, DataDeviceUserData,
                DataSourceUserData, ServerDndGrabHandler, clear_data_device_selection,
                set_data_device_focus,
            },
            primary_selection::{
                PrimaryDeviceManagerGlobalData, PrimaryDeviceUserData, PrimarySelectionHandler,
                PrimarySelectionState, PrimarySourceUserData, clear_primary_selection,
                set_primary_focus,
            },
        },
        shell::wlr_layer::{
            KeyboardInteractivity, Layer as WlrLayer, LayerSurface as WlrLayerSurface,
            LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState,
        },
        shell::xdg::{
            PopupSurface, PositionerState, ShellClient,
            SurfaceCachedState as XdgSurfaceCachedState, ToplevelSurface, XdgShellHandler,
            XdgShellState, XdgToplevelSurfaceData,
            decoration::{XdgDecorationHandler, XdgDecorationState},
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
        tablet_manager::{
            TabletDescriptor, TabletManagerState, TabletSeatHandler, TabletSeatTrait,
            TabletSeatUserData, TabletToolUserData, TabletUserData,
        },
        viewporter::ViewporterState,
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
    },
};
use text::TextRenderer;
use thiserror::Error;
use tracing::{debug, info, warn};
use wayland_protocols::ext::workspace::v1::server::{
    ext_workspace_group_handle_v1, ext_workspace_handle_v1, ext_workspace_manager_v1,
};
use wayland_protocols::wp::primary_selection::zv1::server::{
    zwp_primary_selection_device_manager_v1::{
        self as primary_device_manager, ZwpPrimarySelectionDeviceManagerV1,
    },
    zwp_primary_selection_device_v1::{self as primary_device, ZwpPrimarySelectionDeviceV1},
    zwp_primary_selection_source_v1::{self as primary_source, ZwpPrimarySelectionSourceV1},
};
use wayland_protocols::wp::{
    cursor_shape::v1::server::{
        wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
        wp_cursor_shape_manager_v1::{self as cursor_shape_manager, WpCursorShapeManagerV1},
    },
    keyboard_shortcuts_inhibit::zv1::server::{
        zwp_keyboard_shortcuts_inhibit_manager_v1::{
            self as shortcuts_inhibit_manager, ZwpKeyboardShortcutsInhibitManagerV1,
        },
        zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
    },
    pointer_constraints::zv1::server::{
        zwp_confined_pointer_v1::ZwpConfinedPointerV1,
        zwp_locked_pointer_v1::ZwpLockedPointerV1,
        zwp_pointer_constraints_v1::{
            self as pointer_constraints_manager, ZwpPointerConstraintsV1,
        },
    },
    pointer_gestures::zv1::server::{
        zwp_pointer_gesture_hold_v1::ZwpPointerGestureHoldV1,
        zwp_pointer_gesture_pinch_v1::ZwpPointerGesturePinchV1,
        zwp_pointer_gesture_swipe_v1::ZwpPointerGestureSwipeV1,
        zwp_pointer_gestures_v1::{self as pointer_gestures_manager, ZwpPointerGesturesV1},
    },
    presentation_time::server::{wp_presentation, wp_presentation_feedback},
    relative_pointer::zv1::server::{
        zwp_relative_pointer_manager_v1::{
            self as relative_pointer_manager, ZwpRelativePointerManagerV1,
        },
        zwp_relative_pointer_v1::ZwpRelativePointerV1,
    },
    tablet::zv2::server::{
        zwp_tablet_manager_v2::{self as tablet_manager, ZwpTabletManagerV2},
        zwp_tablet_seat_v2::ZwpTabletSeatV2,
        zwp_tablet_tool_v2::ZwpTabletToolV2,
        zwp_tablet_v2::ZwpTabletV2,
    },
};
use x11rb::{
    connection::Connection as _,
    protocol::Event as X11Event,
    protocol::xproto::{
        ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, ImageFormat, WindowClass,
    },
    rust_connection::RustConnection,
};

/// Exact Smithay release selected for the experimental backend.
pub const SMITHAY_VERSION: &str = "0.7.0";

/// linux-dmabuf version published by Smithay's default-feedback global.
pub const LINUX_DMABUF_VERSION: u32 = 5;

/// linux-drm-syncobj version published when the DRM device supports eventfd waits.
pub const LINUX_DRM_SYNCOBJ_VERSION: u32 = 1;

/// Combined connection-lifetime ceiling for relative-pointer and pointer-constraint objects.
pub const MAX_CLIENT_POINTER_EXTENSION_OBJECTS: usize = 64;

/// Connection-lifetime ceiling for pointer gesture objects.
pub const MAX_CLIENT_POINTER_GESTURES: usize = 64;

/// Connection-lifetime ceiling for cursor-shape device objects.
pub const MAX_CLIENT_CURSOR_SHAPES: usize = 64;

/// Connection-lifetime ceiling for `wl_touch` objects.
pub const MAX_CLIENT_TOUCH_DEVICES: usize = 16;

/// Connection-lifetime ceiling for tablet-seat protocol objects.
pub const MAX_CLIENT_TABLET_SEATS: usize = 16;

/// Maximum physical tablet devices retained by one compositor seat.
pub const MAX_TABLET_DEVICES: usize = 16;

/// Maximum tablet tools retained by one compositor seat.
pub const MAX_TABLET_TOOLS: usize = 64;

/// Advertised `zwp_tablet_manager_v2` protocol version.
pub const TABLET_MANAGER_VERSION: u32 = 1;

/// Connection-lifetime ceiling for presentation feedback objects.
pub const MAX_CLIENT_PRESENTATION_FEEDBACKS: usize = 256;

/// Connection-lifetime ceiling for keyboard-shortcut inhibitor objects.
pub const MAX_CLIENT_SHORTCUT_INHIBITORS: usize = 64;

/// Advertised `zwp_keyboard_shortcuts_inhibit_manager_v1` protocol version.
pub const KEYBOARD_SHORTCUTS_INHIBIT_VERSION: u32 = 1;

/// Advertised `wp_cursor_shape_manager_v1` protocol version.
pub const CURSOR_SHAPE_VERSION: u32 = 2;

/// Advertised `wp_presentation` protocol version.
pub const PRESENTATION_VERSION: u32 = 2;

/// Configuration for the managed nested-X11 Wayland backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NestedOptions {
    /// Parent X11 display; `None` uses `DISPLAY`.
    pub display: Option<String>,
    /// Name to create below `XDG_RUNTIME_DIR`.
    pub socket_name: String,
    /// Stop after this many clients have disconnected; zero runs until closed.
    pub exit_after_disconnects: usize,
    /// Renderer preference; automatic selection prefers GLES2 and falls back to Pixman.
    pub renderer: RendererKind,
}

impl Default for NestedOptions {
    fn default() -> Self {
        Self {
            display: None,
            socket_name: format!("nobox-wayland-{}", std::process::id()),
            exit_after_disconnects: 0,
            renderer: RendererKind::Auto,
        }
    }
}

/// Renderer used by the nested compositor.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RendererKind {
    /// Prefer GLES2 and fall back to Pixman when EGL is unavailable.
    #[default]
    Auto,
    /// Require Smithay's GLES2 renderer.
    Gles2,
    /// Require Smithay's software Pixman renderer.
    Pixman,
}

/// Environment diagnostics for the managed nested-X11 backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NestedDiagnostics {
    /// Selected X11 display.
    pub display: String,
    /// Private runtime directory that will contain the Wayland socket.
    pub runtime_dir: PathBuf,
}

impl NestedDiagnostics {
    /// Validate the process environment needed by the managed nested backend.
    pub fn inspect(display_override: Option<&str>) -> Result<Self, WaylandError> {
        let display = display_override
            .map(str::to_owned)
            .or_else(|| std::env::var("DISPLAY").ok())
            .filter(|value| !value.is_empty())
            .ok_or(WaylandError::MissingDisplay)?;
        RustConnection::connect(Some(&display)).map_err(|error| {
            WaylandError::ParentDisplayUnavailable {
                display: display.clone(),
                reason: error.to_string(),
            }
        })?;
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or(WaylandError::MissingRuntimeDirectory)?;
        validate_runtime_dir(&runtime_dir)?;
        Ok(Self {
            display,
            runtime_dir,
        })
    }
}

/// Result of a completed nested compositor run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunReport {
    /// Wayland socket that was served.
    pub socket_name: OsString,
    /// Number of successfully rendered frames.
    pub rendered_frames: usize,
    /// Number of disconnected clients observed before shutdown.
    pub disconnected_clients: usize,
    /// Renderer that actually presented the nested session.
    pub renderer: RendererKind,
    snapshot: SessionSnapshot,
    disposition: RunDisposition,
}

impl RunReport {
    /// Separates the captured session state from the requested process action.
    #[must_use]
    pub fn into_parts(self) -> (SessionSnapshot, RunDisposition) {
        (self.snapshot, self.disposition)
    }
}

/// Errors produced by the experimental Wayland infrastructure.
#[derive(Debug, Error)]
pub enum WaylandError {
    /// No nested X server was selected.
    #[error("DISPLAY is unset; the managed Wayland backend requires a nested X server")]
    MissingDisplay,
    /// The selected nested X11 display cannot be inspected.
    #[error("nested X11 display {display} is unavailable: {reason}")]
    ParentDisplayUnavailable {
        /// Rejected display name.
        display: String,
        /// X11 connection error.
        reason: String,
    },
    /// No Wayland runtime directory was selected.
    #[error("XDG_RUNTIME_DIR is unset")]
    MissingRuntimeDirectory,
    /// The selected runtime directory is unsuitable for a private socket.
    #[error("invalid XDG_RUNTIME_DIR {path}: {reason}")]
    InvalidRuntimeDirectory {
        /// Rejected path.
        path: PathBuf,
        /// Human-readable reason.
        reason: String,
    },
    /// The requested socket name could escape the runtime directory.
    #[error("invalid Wayland socket name `{0}`; use one basename of at most 64 bytes")]
    InvalidSocketName(String),
    /// Smithay, the selected host/device backend, or calloop could not initialize.
    #[error("could not initialize Wayland backend: {0}")]
    Initialization(String),
    /// The compositor event loop failed.
    #[error("Wayland event loop failed: {0}")]
    EventLoop(String),
    /// The selected Wayland renderer failed.
    #[error("Wayland renderer failed: {0}")]
    Renderer(String),
    /// The protocol-neutral runtime-control endpoint failed.
    #[error("Wayland runtime control failed: {0}")]
    RuntimeControl(#[from] nobox_runtime::ControlError),
}

/// Run the managed Wayland shell in a window on the selected X11 display.
pub fn run_nested(options: NestedOptions) -> Result<RunReport, WaylandError> {
    run_nested_with_control(options, |_| Ok::<(), std::convert::Infallible>(()))
}

/// Runs the nested backend and hands its live neutral control sender to a process-level owner.
///
/// # Errors
///
/// Returns an error if nested X11, Smithay, rendering, Wayland dispatch, the
/// private runtime endpoint, or the process-level control owner cannot start.
pub fn run_nested_with_control<G, E>(
    options: NestedOptions,
    control_ready: impl FnOnce(ControlSender) -> Result<G, E>,
) -> Result<RunReport, WaylandError>
where
    E: std::fmt::Display,
{
    run_nested_with_config(options, Config::default(), control_ready, || {
        Ok::<Config, std::convert::Infallible>(Config::default())
    })
}

/// Runs the nested backend with validated desktop configuration and live reload.
///
/// A failed reload leaves the last applied configuration and every client
/// intact. The reload callback is invoked only after a neutral runtime reload
/// request reaches the compositor loop.
///
/// # Errors
///
/// Returns an error if backend startup or dispatch fails. Reload errors are
/// logged and do not stop the compositor.
pub fn run_nested_with_config<G, E, R, RE>(
    options: NestedOptions,
    config: Config,
    control_ready: impl FnOnce(ControlSender) -> Result<G, E>,
    reload_config: R,
) -> Result<RunReport, WaylandError>
where
    E: std::fmt::Display,
    R: FnMut() -> Result<Config, RE>,
    RE: std::fmt::Display,
{
    run_nested_with_session(
        options,
        config,
        SessionRestore::default(),
        control_ready,
        reload_config,
        |_| false,
    )
}

/// Runs the nested backend with validated configuration and session handoff.
///
/// The restore candidates are duplicate-safe and consumed at most once. A
/// runtime save request is serviced at an event-loop boundary with one coherent
/// protocol-neutral snapshot.
///
/// # Errors
///
/// Returns an error if backend startup or dispatch fails. Reload and requested
/// session-save failures are logged without stopping the compositor.
pub fn run_nested_with_session<G, E, R, RE, S>(
    options: NestedOptions,
    config: Config,
    restore: SessionRestore,
    control_ready: impl FnOnce(ControlSender) -> Result<G, E>,
    mut reload_config: R,
    mut save_session: S,
) -> Result<RunReport, WaylandError>
where
    E: std::fmt::Display,
    R: FnMut() -> Result<Config, RE>,
    RE: std::fmt::Display,
    S: FnMut(&SessionSnapshot) -> bool,
{
    validate_socket_name(&options.socket_name)?;
    NestedDiagnostics::inspect(options.display.as_deref())?;

    let mut event_loop = EventLoop::<LoopData>::try_new()
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    let mut display = Display::<Compositor>::new()
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    let display_handle = display.handle();
    let disconnected = Arc::new(AtomicUsize::new(0));
    let mut nested_window = NestedRenderer::create(options.renderer, options.display.as_deref())?;
    let renderer = nested_window.kind();
    let mode = OutputMode {
        size: nested_window.size(),
        refresh: 60_000,
    };
    let output = Output::new(
        "nobox-1".to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Nobox".to_owned(),
            model: "Nested output".to_owned(),
        },
    );
    let _global = output.create_global::<Compositor>(&display_handle);
    output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
    output.set_preferred(mode);
    let compositor = Compositor::new(
        &display_handle,
        output,
        nested_window.size(),
        config,
        options.socket_name.clone().into(),
        restore,
    );
    let mut data = LoopData {
        compositor,
        display_handle: display_handle.clone(),
        display_ready: false,
        rendered_frames: 1,
        fatal_error: None,
        running: true,
        reload_requested: false,
        session_save_requested: false,
        runtime_control: None,
    };

    let (runtime_wake, runtime_events) = channel::channel();
    let runtime_control = ControlServer::bind(BackendKind::Wayland, move || {
        let _ = runtime_wake.send(());
    })?;
    let _control_guard = control_ready(runtime_control.sender())
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    data.runtime_control = Some(runtime_control);
    event_loop
        .handle()
        .insert_source(runtime_events, |event, _, loop_data| {
            if !matches!(event, ChannelEvent::Msg(())) {
                return;
            }
            let requests = loop_data
                .runtime_control
                .as_ref()
                .map(|control| control.drain().collect::<Vec<_>>())
                .unwrap_or_default();
            for request in requests {
                match request {
                    ControlRequest::Reload => loop_data.reload_requested = true,
                    ControlRequest::Shutdown => loop_data.running = false,
                    ControlRequest::SaveSession => loop_data.session_save_requested = true,
                }
            }
        })
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;

    let listener = ListeningSocketSource::with_name(&options.socket_name)
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    let socket_name = listener.socket_name().to_os_string();
    let client_disconnects = Arc::clone(&disconnected);
    event_loop
        .handle()
        .insert_source(listener, move |stream, _, loop_data| {
            let disconnected_client_ids = Arc::clone(&loop_data.compositor.disconnected_client_ids);
            let client_data = Arc::new(WaylandClientState {
                compositor_state: CompositorClientState::default(),
                disconnected: Arc::clone(&client_disconnects),
                surface_count: Arc::new(AtomicUsize::new(0)),
                selection_source_count: Arc::new(AtomicUsize::new(0)),
                selection_device_count: Arc::new(AtomicUsize::new(0)),
                pointer_extension_count: Arc::new(AtomicUsize::new(0)),
                pointer_gesture_count: Arc::new(AtomicUsize::new(0)),
                cursor_shape_count: Arc::new(AtomicUsize::new(0)),
                touch_device_count: Arc::new(AtomicUsize::new(0)),
                tablet_seat_count: Arc::new(AtomicUsize::new(0)),
                presentation_feedback_count: Arc::new(AtomicUsize::new(0)),
                shortcut_inhibitor_count: Arc::new(AtomicUsize::new(0)),
                disconnected_client_ids,
            });
            if let Err(error) = loop_data.display_handle.insert_client(stream, client_data) {
                loop_data.fail(format!("could not register Wayland client: {error}"));
            }
        })
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;

    let display_fd = display
        .as_fd()
        .try_clone_to_owned()
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(
            Generic::new(display_fd, Interest::READ, Mode::Level),
            |_, _, loop_data| {
                loop_data.display_ready = true;
                Ok(PostAction::Continue)
            },
        )
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;

    println!("ready: {}", socket_name.to_string_lossy());

    while data.running {
        event_loop
            .dispatch(Duration::from_millis(250), &mut data)
            .map_err(|error| WaylandError::EventLoop(error.to_string()))?;
        if data.display_ready {
            data.display_ready = false;
            display
                .dispatch_clients(&mut data.compositor)
                .map_err(|error| WaylandError::EventLoop(error.to_string()))?;
        }
        data.compositor.cleanup_disconnected_selection_owners();
        nested_window.dispatch_input(&mut data.compositor)?;
        if std::mem::take(&mut data.compositor.reload_requested) {
            data.reload_requested = true;
        }
        if data.compositor.exit_requested {
            data.running = false;
        }
        data.compositor.check_client_liveness();
        if data.compositor.redraw_needed {
            nested_window.present(&mut data.compositor)?;
            data.rendered_frames = data.rendered_frames.saturating_add(1);
        }
        display
            .flush_clients()
            .map_err(|error| WaylandError::EventLoop(error.to_string()))?;
        if std::mem::take(&mut data.reload_requested) {
            info!("Wayland shell received a configuration reload request");
            match reload_config() {
                Ok(config) => {
                    data.compositor.apply_config(config);
                    info!("Wayland shell applied a configuration reload");
                }
                Err(error) => {
                    warn!(%error, "Wayland configuration reload rejected; retaining last good configuration");
                }
            }
        }
        if std::mem::take(&mut data.session_save_requested) {
            let snapshot = data.compositor.session_snapshot();
            if save_session(&snapshot) {
                info!("external Wayland session snapshot completed");
            } else {
                warn!("external Wayland session snapshot failed");
            }
        }
        if let Some(error) = data.fatal_error.take() {
            return Err(WaylandError::EventLoop(error));
        }
        let disconnected_clients = disconnected.load(Ordering::Acquire);
        if options.exit_after_disconnects > 0
            && disconnected_clients >= options.exit_after_disconnects
        {
            break;
        }
    }

    let snapshot = data.compositor.session_snapshot();
    Ok(RunReport {
        socket_name,
        rendered_frames: data.rendered_frames,
        disconnected_clients: disconnected.load(Ordering::Acquire),
        renderer,
        snapshot,
        disposition: data.compositor.disposition.clone(),
    })
}

enum NestedRenderer {
    Gles2(Box<GlesNestedWindow>),
    Pixman(Box<NestedX11Window>),
}

impl NestedRenderer {
    fn create(kind: RendererKind, display: Option<&str>) -> Result<Self, WaylandError> {
        let process_display = std::env::var("DISPLAY").ok();
        let winit_can_use_display = display.is_none_or(|requested| {
            process_display
                .as_deref()
                .is_some_and(|active| active == requested)
        });
        if kind != RendererKind::Pixman && winit_can_use_display {
            match GlesNestedWindow::create() {
                Ok(window) => return Ok(Self::Gles2(Box::new(window))),
                Err(error) if kind == RendererKind::Gles2 => return Err(error),
                Err(error) => warn!(%error, "GLES2 unavailable; falling back to Pixman"),
            }
        } else if kind == RendererKind::Gles2 {
            return Err(WaylandError::Initialization(
                "--renderer gles2 requires --display to match DISPLAY".to_owned(),
            ));
        }
        Ok(Self::Pixman(Box::new(NestedX11Window::create(display)?)))
    }

    fn size(&self) -> smithay::utils::Size<i32, smithay::utils::Physical> {
        match self {
            Self::Gles2(window) => window.size(),
            Self::Pixman(window) => window.size,
        }
    }

    fn kind(&self) -> RendererKind {
        match self {
            Self::Gles2(_) => RendererKind::Gles2,
            Self::Pixman(_) => RendererKind::Pixman,
        }
    }

    fn dispatch_input(&mut self, compositor: &mut Compositor) -> Result<(), WaylandError> {
        match self {
            Self::Gles2(window) => window.dispatch_input(compositor),
            Self::Pixman(window) => window.dispatch_input(compositor),
        }
    }

    fn present(&mut self, compositor: &mut Compositor) -> Result<(), WaylandError> {
        match self {
            Self::Gles2(window) => window.present(compositor),
            Self::Pixman(window) => window.present(compositor),
        }
    }
}

struct GlesNestedWindow {
    backend: WinitGraphicsBackend<GlesRenderer>,
    event_loop: WinitEventLoop,
}

impl Drop for GlesNestedWindow {
    fn drop(&mut self) {
        // Winit permits only one event-loop initialization per process on this
        // nested path. Hide the old host window before an in-process restart
        // falls back to the independently owned Pixman/X11 host.
        self.backend.window().set_visible(false);
    }
}

impl GlesNestedWindow {
    fn create() -> Result<Self, WaylandError> {
        let (backend, event_loop) = winit::init::<GlesRenderer>()
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        Ok(Self {
            backend,
            event_loop,
        })
    }

    fn size(&self) -> smithay::utils::Size<i32, smithay::utils::Physical> {
        self.backend.window_size()
    }

    fn dispatch_input(&mut self, compositor: &mut Compositor) -> Result<(), WaylandError> {
        enum Event {
            Motion(f64, f64, u32),
            Button(u32, ButtonState, u32),
            Axis(AxisFrame),
            Key(Keycode, KeyState, u32),
            TouchDown(smithay::backend::input::TouchSlot, f64, f64, u32),
            TouchMotion(smithay::backend::input::TouchSlot, f64, f64, u32),
            TouchUp(smithay::backend::input::TouchSlot, u32),
            TouchCancel,
            TouchFrame,
            Resize(smithay::utils::Size<i32, Physical>),
            Close,
            Redraw,
        }
        let mut events = Vec::new();
        let _ =
            self.event_loop.dispatch_new_events(|event| match event {
                WinitEvent::Input(InputEvent::PointerMotionAbsolute { event }) => {
                    events.push(Event::Motion(event.x(), event.y(), event.time_msec()));
                }
                WinitEvent::Input(InputEvent::PointerButton { event }) => events.push(
                    Event::Button(event.button_code(), event.state(), event.time_msec()),
                ),
                WinitEvent::Input(InputEvent::PointerAxis { event }) => {
                    let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
                    for axis in [Axis::Horizontal, Axis::Vertical] {
                        frame = frame.relative_direction(axis, event.relative_direction(axis));
                        if let Some(amount) = event.amount(axis) {
                            frame = frame.value(axis, amount);
                        }
                        if let Some(amount) = event.amount_v120(axis) {
                            frame = frame.v120(axis, amount.round() as i32);
                        }
                    }
                    events.push(Event::Axis(frame));
                }
                WinitEvent::Input(InputEvent::Keyboard { event }) => events.push(Event::Key(
                    event.key_code(),
                    event.state(),
                    event.time_msec(),
                )),
                WinitEvent::Input(InputEvent::TouchDown { event }) => events.push(
                    Event::TouchDown(event.slot(), event.x(), event.y(), event.time_msec()),
                ),
                WinitEvent::Input(InputEvent::TouchMotion { event }) => events.push(
                    Event::TouchMotion(event.slot(), event.x(), event.y(), event.time_msec()),
                ),
                WinitEvent::Input(InputEvent::TouchUp { event }) => {
                    events.push(Event::TouchUp(event.slot(), event.time_msec()));
                }
                WinitEvent::Input(InputEvent::TouchCancel { .. }) => {
                    events.push(Event::TouchCancel);
                }
                WinitEvent::Input(InputEvent::TouchFrame { .. }) => events.push(Event::TouchFrame),
                WinitEvent::CloseRequested => events.push(Event::Close),
                WinitEvent::Resized { size, .. } => events.push(Event::Resize(size)),
                WinitEvent::Redraw => events.push(Event::Redraw),
                _ => {}
            });
        for event in events {
            match event {
                Event::Motion(x, y, time) => compositor.pointer_motion_nested(x, y, time),
                Event::Button(button, state, time) => {
                    compositor.pointer_button_code(button, state, time);
                }
                Event::Axis(frame) => compositor.pointer_axis(frame),
                Event::Key(key, state, time) => compositor.keyboard_keycode(key, state, time),
                Event::TouchDown(slot, x, y, time) => {
                    compositor.touch_down((x, y).into(), slot, time);
                }
                Event::TouchMotion(slot, x, y, time) => {
                    compositor.touch_motion((x, y).into(), slot, time);
                }
                Event::TouchUp(slot, time) => compositor.touch_up(slot, time),
                Event::TouchCancel => compositor.touch_cancel(),
                Event::TouchFrame => compositor.touch_frame(),
                Event::Resize(size) => compositor.resize_output(size),
                Event::Close => compositor.exit_requested = true,
                Event::Redraw => compositor.redraw_needed = true,
            }
        }
        Ok(())
    }

    fn present(&mut self, compositor: &mut Compositor) -> Result<(), WaylandError> {
        let size = self.backend.window_size();
        let damage = Rectangle::from_size(size);
        let region: Rectangle<i32, Logical> = Rectangle::from_size((size.w, size.h).into());
        compositor.refresh_scene();
        {
            let (renderer, mut framebuffer) = self
                .backend
                .bind()
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            let mut elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = compositor
                .space
                .render_elements_for_region(renderer, &region, 1.0, 1.0);
            if let Some((surface, location)) = compositor.cursor_surface_location() {
                elements.extend(render_elements_from_surface_tree(
                    renderer,
                    &surface,
                    location,
                    1.0,
                    1.0,
                    Kind::Cursor,
                ));
            }
            if let Some((surface, location)) = compositor.dnd_icon_surface_location() {
                elements.extend(render_elements_from_surface_tree(
                    renderer,
                    &surface,
                    location,
                    1.0,
                    1.0,
                    Kind::Cursor,
                ));
            }
            let decorations = compositor.decoration_elements();
            let overlays = compositor.overlay_elements();
            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Flipped180)
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            frame
                .clear(Color32F::new(0.08, 0.10, 0.14, 1.0), &[damage])
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            draw_render_elements::<GlesRenderer, _, _>(&mut frame, 1.0, &decorations, &[damage])
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            draw_render_elements::<GlesRenderer, _, _>(&mut frame, 1.0, &overlays, &[damage])
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            let _ = frame
                .finish()
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        }
        self.backend
            .submit(Some(&[damage]))
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        compositor.finish_frame_callbacks();
        Ok(())
    }
}

struct NestedX11Window {
    connection: RustConnection,
    window: u32,
    graphics_context: u32,
    depth: u8,
    size: smithay::utils::Size<i32, smithay::utils::Physical>,
}

impl NestedX11Window {
    fn create(display: Option<&str>) -> Result<Self, WaylandError> {
        const WIDTH: u16 = 640;
        const HEIGHT: u16 = 360;
        let buffer_size: smithay::utils::Size<i32, smithay::utils::Buffer> =
            (i32::from(WIDTH), i32::from(HEIGHT)).into();
        let physical_size: smithay::utils::Size<i32, smithay::utils::Physical> =
            (i32::from(WIDTH), i32::from(HEIGHT)).into();
        let mut renderer =
            PixmanRenderer::new().map_err(|error| WaylandError::Renderer(error.to_string()))?;
        let mut image = renderer
            .create_buffer(Fourcc::Xrgb8888, buffer_size)
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        let mut framebuffer = renderer
            .bind(&mut image)
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        {
            let mut frame = renderer
                .render(&mut framebuffer, physical_size, Transform::Normal)
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            frame
                .clear(
                    Color32F::new(0.08, 0.10, 0.14, 1.0),
                    &[Rectangle::from_size(physical_size)],
                )
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            frame
                .finish()
                .map_err(|error| WaylandError::Renderer(error.to_string()))?
                .wait()
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        }
        let mapping = renderer
            .copy_framebuffer(
                &framebuffer,
                Rectangle::from_size(buffer_size),
                Fourcc::Xrgb8888,
            )
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        let pixels = renderer
            .map_texture(&mapping)
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;

        let (connection, screen_index) = RustConnection::connect(display)
            .map_err(|error| WaylandError::Initialization(error.to_string()))?;
        let screen = &connection.setup().roots[screen_index];
        let depth = screen.root_depth;
        if screen.root_depth != 24 && screen.root_depth != 32 {
            return Err(WaylandError::Initialization(format!(
                "nested X11 root depth {} is unsupported",
                screen.root_depth
            )));
        }
        let window = connection
            .generate_id()
            .map_err(|error| WaylandError::Initialization(error.to_string()))?;
        let graphics_context = connection
            .generate_id()
            .map_err(|error| WaylandError::Initialization(error.to_string()))?;
        connection
            .create_window(
                screen.root_depth,
                window,
                screen.root,
                0,
                0,
                WIDTH,
                HEIGHT,
                0,
                WindowClass::INPUT_OUTPUT,
                screen.root_visual,
                &CreateWindowAux::new()
                    .background_pixel(screen.black_pixel)
                    .event_mask(
                        EventMask::EXPOSURE
                            | EventMask::STRUCTURE_NOTIFY
                            | EventMask::POINTER_MOTION
                            | EventMask::BUTTON_PRESS
                            | EventMask::BUTTON_RELEASE
                            | EventMask::KEY_PRESS
                            | EventMask::KEY_RELEASE
                            | EventMask::FOCUS_CHANGE,
                    ),
            )
            .map_err(|error| WaylandError::Initialization(error.to_string()))?;
        connection
            .create_gc(graphics_context, window, &CreateGCAux::new())
            .map_err(|error| WaylandError::Initialization(error.to_string()))?;
        connection
            .map_window(window)
            .map_err(|error| WaylandError::Initialization(error.to_string()))?;
        connection
            .put_image(
                ImageFormat::Z_PIXMAP,
                window,
                graphics_context,
                WIDTH,
                HEIGHT,
                0,
                0,
                0,
                screen.root_depth,
                pixels,
            )
            .map_err(|error| WaylandError::Initialization(error.to_string()))?;
        connection
            .flush()
            .map_err(|error| WaylandError::Initialization(error.to_string()))?;
        Ok(Self {
            connection,
            window,
            graphics_context,
            depth,
            size: physical_size,
        })
    }

    fn present(&mut self, compositor: &mut Compositor) -> Result<(), WaylandError> {
        let buffer_size: smithay::utils::Size<i32, smithay::utils::Buffer> =
            (self.size.w, self.size.h).into();
        let mut renderer =
            PixmanRenderer::new().map_err(|error| WaylandError::Renderer(error.to_string()))?;
        let mut image = renderer
            .create_buffer(Fourcc::Xrgb8888, buffer_size)
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        let mut framebuffer = renderer
            .bind(&mut image)
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        let damage = Rectangle::from_size(self.size);
        let region: Rectangle<i32, Logical> =
            Rectangle::from_size((self.size.w, self.size.h).into());
        compositor.refresh_scene();
        let mut elements: Vec<WaylandSurfaceRenderElement<PixmanRenderer>> = compositor
            .space
            .render_elements_for_region(&mut renderer, &region, 1.0, 1.0);
        if let Some((surface, location)) = compositor.cursor_surface_location() {
            elements.extend(render_elements_from_surface_tree(
                &mut renderer,
                &surface,
                location,
                1.0,
                1.0,
                Kind::Cursor,
            ));
        }
        if let Some((surface, location)) = compositor.dnd_icon_surface_location() {
            elements.extend(render_elements_from_surface_tree(
                &mut renderer,
                &surface,
                location,
                1.0,
                1.0,
                Kind::Cursor,
            ));
        }
        let decorations = compositor.decoration_elements();
        let overlays = compositor.overlay_elements();
        {
            let mut frame = renderer
                .render(&mut framebuffer, self.size, Transform::Normal)
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            frame
                .clear(Color32F::new(0.08, 0.10, 0.14, 1.0), &[damage])
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            draw_render_elements::<PixmanRenderer, _, _>(&mut frame, 1.0, &decorations, &[damage])
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            draw_render_elements::<PixmanRenderer, _, _>(&mut frame, 1.0, &overlays, &[damage])
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            frame
                .finish()
                .map_err(|error| WaylandError::Renderer(error.to_string()))?
                .wait()
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        }
        let mapping = renderer
            .copy_framebuffer(
                &framebuffer,
                Rectangle::from_size(buffer_size),
                Fourcc::Xrgb8888,
            )
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        let pixels = renderer
            .map_texture(&mapping)
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        self.connection
            .put_image(
                ImageFormat::Z_PIXMAP,
                self.window,
                self.graphics_context,
                u16::try_from(self.size.w).unwrap_or(u16::MAX),
                u16::try_from(self.size.h).unwrap_or(u16::MAX),
                0,
                0,
                0,
                self.depth,
                pixels,
            )
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        self.connection
            .flush()
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        compositor.finish_frame_callbacks();
        Ok(())
    }

    fn dispatch_input(&mut self, compositor: &mut Compositor) -> Result<(), WaylandError> {
        while let Some(event) = self
            .connection
            .poll_for_event()
            .map_err(|error| WaylandError::EventLoop(error.to_string()))?
        {
            match event {
                X11Event::MotionNotify(event) => compositor.pointer_motion_nested(
                    f64::from(event.event_x),
                    f64::from(event.event_y),
                    event.time,
                ),
                X11Event::ButtonPress(event) => {
                    compositor.pointer_button(event.detail, ButtonState::Pressed, event.time);
                }
                X11Event::ButtonRelease(event) => {
                    compositor.pointer_button(event.detail, ButtonState::Released, event.time);
                }
                X11Event::KeyPress(event) => {
                    compositor.keyboard_key(event.detail, KeyState::Pressed, event.time);
                }
                X11Event::KeyRelease(event) => {
                    compositor.keyboard_key(event.detail, KeyState::Released, event.time);
                }
                X11Event::ConfigureNotify(event) => {
                    self.size = (i32::from(event.width), i32::from(event.height)).into();
                    compositor.resize_output(self.size);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn send_surface_callbacks(
    surface: &WlSurface,
    frame_time: u32,
    output: &Output,
    presented_at: Duration,
    refresh: Refresh,
    sequence: u64,
    flags: wp_presentation_feedback::Kind,
) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surface, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(frame_time);
            }
            for feedback in std::mem::take(
                &mut states
                    .cached_state
                    .get::<PresentationFeedbackCachedState>()
                    .current()
                    .callbacks,
            ) {
                feedback.presented(output, presented_at, refresh, sequence, flags);
            }
        },
        |_, _, &()| true,
    );
}

fn monotonic_time() -> Duration {
    let time = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    Duration::new(
        u64::try_from(time.tv_sec).unwrap_or_default(),
        u32::try_from(time.tv_nsec).unwrap_or_default(),
    )
}

fn presentation_refresh(output: &Output) -> Refresh {
    output.current_mode().map_or(Refresh::Unknown, |mode| {
        let nanos = 1_000_000_000_000_u64
            .checked_div(u64::try_from(mode.refresh).unwrap_or_default())
            .unwrap_or_default();
        if nanos == 0 {
            Refresh::Unknown
        } else {
            Refresh::fixed(Duration::from_nanos(nanos))
        }
    })
}

fn validate_runtime_dir(path: &Path) -> Result<(), WaylandError> {
    let metadata = fs::metadata(path).map_err(|error| WaylandError::InvalidRuntimeDirectory {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    if !metadata.is_dir() {
        return Err(WaylandError::InvalidRuntimeDirectory {
            path: path.to_path_buf(),
            reason: "not a directory".to_owned(),
        });
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(WaylandError::InvalidRuntimeDirectory {
            path: path.to_path_buf(),
            reason: format!("mode is {mode:#o}, expected 0o700"),
        });
    }
    if metadata.uid()
        != fs::metadata("/proc/self")
            .map_err(|error| WaylandError::InvalidRuntimeDirectory {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?
            .uid()
    {
        return Err(WaylandError::InvalidRuntimeDirectory {
            path: path.to_path_buf(),
            reason: "directory is not owned by the current user".to_owned(),
        });
    }
    Ok(())
}

fn validate_socket_name(name: &str) -> Result<(), WaylandError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\0');
    if valid {
        Ok(())
    } else {
        Err(WaylandError::InvalidSocketName(name.to_owned()))
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

fn bounded_protocol_text(value: Option<&str>, maximum_bytes: usize) -> String {
    let value = value.unwrap_or_default();
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_owned()
}

fn coordinate_end(start: i32, length: u32) -> i32 {
    start.saturating_add(i32::try_from(length.saturating_sub(1)).unwrap_or(i32::MAX))
}

fn work_area_from_nonexclusive_zone(
    output: Geometry,
    zone: Rectangle<i32, Logical>,
    margins: MarginConfig,
) -> Geometry {
    let output_right = i64::from(output.x) + i64::from(output.width);
    let output_bottom = i64::from(output.y) + i64::from(output.height);
    let zone_right = i64::from(zone.loc.x) + i64::from(zone.size.w.max(0));
    let zone_bottom = i64::from(zone.loc.y) + i64::from(zone.size.h.max(0));
    let horizontal_end = coordinate_end(output.x, output.width);
    let vertical_end = coordinate_end(output.y, output.height);
    let layer_area = output.work_area([EdgeReservations {
        left: EdgeReservation {
            depth: u32::try_from(zone.loc.x.saturating_sub(output.x)).unwrap_or(0),
            start: output.y,
            end: vertical_end,
        },
        right: EdgeReservation {
            depth: u32::try_from(output_right.saturating_sub(zone_right)).unwrap_or(0),
            start: output.y,
            end: vertical_end,
        },
        top: EdgeReservation {
            depth: u32::try_from(zone.loc.y.saturating_sub(output.y)).unwrap_or(0),
            start: output.x,
            end: horizontal_end,
        },
        bottom: EdgeReservation {
            depth: u32::try_from(output_bottom.saturating_sub(zone_bottom)).unwrap_or(0),
            start: output.x,
            end: horizontal_end,
        },
    }]);
    layer_area.work_area([EdgeReservations {
        left: EdgeReservation {
            depth: margins.left,
            start: layer_area.y,
            end: coordinate_end(layer_area.y, layer_area.height),
        },
        right: EdgeReservation {
            depth: margins.right,
            start: layer_area.y,
            end: coordinate_end(layer_area.y, layer_area.height),
        },
        top: EdgeReservation {
            depth: margins.top,
            start: layer_area.x,
            end: coordinate_end(layer_area.x, layer_area.width),
        },
        bottom: EdgeReservation {
            depth: margins.bottom,
            start: layer_area.x,
            end: coordinate_end(layer_area.x, layer_area.width),
        },
    }])
}

fn geometry_end_x(geometry: Geometry) -> i32 {
    geometry
        .x
        .saturating_add(i32::try_from(geometry.width).unwrap_or(i32::MAX))
}

fn geometry_end_y(geometry: Geometry) -> i32 {
    geometry
        .y
        .saturating_add(i32::try_from(geometry.height).unwrap_or(i32::MAX))
}

fn requested_application_dimension(
    amount: Option<nobox_config::PositiveRelativeAmount>,
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

fn placed_application_axis(
    position: AxisPosition,
    bounds_start: i32,
    bounds_length: u32,
    outer_length: u32,
) -> i32 {
    let start = i64::from(bounds_start);
    let available = i64::from(bounds_length.saturating_sub(outer_length));
    let value = match position {
        AxisPosition::Start(offset) => {
            start.saturating_add(i64::from(offset.resolve(bounds_length)))
        }
        AxisPosition::Center => start.saturating_add(available / 2),
        AxisPosition::End(inset) => start
            .saturating_add(available)
            .saturating_sub(i64::from(inset.resolve(bounds_length))),
    };
    i32::try_from(value).unwrap_or(if value.is_negative() {
        i32::MIN
    } else {
        i32::MAX
    })
}

const fn application_layer(layer: ApplicationLayer) -> ClientLayer {
    match layer {
        ApplicationLayer::Below => ClientLayer::Below,
        ApplicationLayer::Normal => ClientLayer::Normal,
        ApplicationLayer::Above => ClientLayer::Above,
    }
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

const fn session_role(role: ClientRole) -> &'static str {
    match role {
        ClientRole::Normal => "normal",
        ClientRole::Dialog => "dialog",
        ClientRole::Utility => "utility",
        ClientRole::Toolbar => "toolbar",
        ClientRole::Menu => "menu",
        ClientRole::Splash => "splash",
        ClientRole::Desktop => "desktop",
        ClientRole::Dock => "dock",
        ClientRole::DropdownMenu => "dropdown_menu",
        ClientRole::PopupMenu => "popup_menu",
        ClientRole::Tooltip => "tooltip",
        ClientRole::Notification => "notification",
        ClientRole::Combo => "combo",
        ClientRole::DragAndDrop => "drag_and_drop",
    }
}

const fn session_layer(layer: ClientLayer) -> SessionLayer {
    match layer {
        ClientLayer::Below => SessionLayer::Below,
        ClientLayer::Normal => SessionLayer::Normal,
        ClientLayer::Above => SessionLayer::Above,
    }
}

const fn restored_layer(layer: SessionLayer) -> ClientLayer {
    match layer {
        SessionLayer::Below => ClientLayer::Below,
        SessionLayer::Normal => ClientLayer::Normal,
        SessionLayer::Above => ClientLayer::Above,
    }
}

const fn session_decoration(decoration: DecorationOverride) -> SessionDecorationOverride {
    match decoration {
        DecorationOverride::Default => SessionDecorationOverride::Default,
        DecorationOverride::Decorated => SessionDecorationOverride::Decorated,
        DecorationOverride::Undecorated => SessionDecorationOverride::Undecorated,
    }
}

const fn restored_decoration(decoration: SessionDecorationOverride) -> DecorationOverride {
    match decoration {
        SessionDecorationOverride::Default => DecorationOverride::Default,
        SessionDecorationOverride::Decorated => DecorationOverride::Decorated,
        SessionDecorationOverride::Undecorated => DecorationOverride::Undecorated,
    }
}

fn axis_placement(position: AxisPosition, reference: u32) -> AxisPlacement {
    match position {
        AxisPosition::Start(amount) => AxisPlacement::Start(amount.resolve(reference)),
        AxisPosition::Center => AxisPlacement::Center,
        AxisPosition::End(amount) => AxisPlacement::End(amount.resolve(reference)),
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

fn binding_cardinal_direction(input: &BindingInput) -> Option<CardinalDirection> {
    if input.has_symbol("Left") {
        Some(CardinalDirection::Left)
    } else if input.has_symbol("Right") {
        Some(CardinalDirection::Right)
    } else if input.has_symbol("Up") {
        Some(CardinalDirection::Up)
    } else if input.has_symbol("Down") {
        Some(CardinalDirection::Down)
    } else {
        None
    }
}

const fn cardinal_direction_is_horizontal(direction: CardinalDirection) -> bool {
    matches!(
        direction,
        CardinalDirection::Left | CardinalDirection::Right
    )
}

const fn cardinal_directions_share_axis(left: CardinalDirection, right: CardinalDirection) -> bool {
    cardinal_direction_is_horizontal(left) == cardinal_direction_is_horizontal(right)
}

fn cardinal_direction_delta(direction: CardinalDirection, step: u32) -> (i32, i32) {
    let step = i32::try_from(step).unwrap_or(i32::MAX);
    match direction {
        CardinalDirection::Left => (-step, 0),
        CardinalDirection::Right => (step, 0),
        CardinalDirection::Up => (0, -step),
        CardinalDirection::Down => (0, step),
    }
}

fn color(color: nobox_config::RgbColor) -> [f32; 4] {
    let pixel = color.pixel();
    [
        f32::from(u8::try_from((pixel >> 16) & 0xff).unwrap_or(0)) / 255.0,
        f32::from(u8::try_from((pixel >> 8) & 0xff).unwrap_or(0)) / 255.0,
        f32::from(u8::try_from(pixel & 0xff).unwrap_or(0)) / 255.0,
        1.0,
    ]
}

fn covered_color(mut value: [f32; 4], coverage: u8) -> [f32; 4] {
    value[3] *= f32::from(coverage) / 255.0;
    value
}

fn text_origin(bounds: Geometry, measured_width: i32, alignment: TitleAlignment) -> i32 {
    let available = i32::try_from(bounds.width).unwrap_or(i32::MAX);
    let offset = match alignment {
        TitleAlignment::Left => 0,
        TitleAlignment::Center => available.saturating_sub(measured_width) / 2,
        TitleAlignment::Right => available.saturating_sub(measured_width),
    };
    bounds.x.saturating_add(offset.max(0))
}

fn horizontal_inset(bounds: Geometry, leading: u32, trailing: u32) -> Option<Geometry> {
    let inset = leading.saturating_add(trailing);
    (inset < bounds.width).then(|| {
        Geometry::new(
            bounds
                .x
                .saturating_add(i32::try_from(leading).unwrap_or(i32::MAX)),
            bounds.y,
            bounds.width.saturating_sub(inset),
            bounds.height,
        )
    })
}

fn centered_axis(origin: i32, available: u32, occupied: u32) -> i32 {
    origin.saturating_add(i32::try_from(available.saturating_sub(occupied) / 2).unwrap_or(i32::MAX))
}

fn place_popup_axis(anchor: i32, origin: i32, available: u32, occupied: u32) -> i32 {
    let maximum = origin
        .saturating_add(i32::try_from(available.saturating_sub(occupied)).unwrap_or(i32::MAX));
    anchor.clamp(origin, maximum)
}

fn focus_cycle_visible_start(total: usize, selected: usize, rows: usize) -> usize {
    if total <= rows || rows == 0 {
        return 0;
    }
    selected.saturating_sub(rows / 2).min(total - rows)
}

fn focus_cycle_modifiers(active: &[KeyboardModifier]) -> Vec<KeyboardModifier> {
    let without_shift = active
        .iter()
        .copied()
        .filter(|modifier| *modifier != KeyboardModifier::Shift)
        .collect::<Vec<_>>();
    if without_shift.is_empty() {
        active.to_vec()
    } else {
        without_shift
    }
}

fn outline_geometries(bounds: Geometry, thickness: u32) -> [Geometry; 4] {
    let horizontal = thickness.min(bounds.height);
    let vertical = thickness.min(bounds.width);
    let right = bounds
        .x
        .saturating_add(i32::try_from(bounds.width.saturating_sub(vertical)).unwrap_or(i32::MAX));
    let bottom = bounds.y.saturating_add(
        i32::try_from(bounds.height.saturating_sub(horizontal)).unwrap_or(i32::MAX),
    );
    [
        Geometry::new(bounds.x, bounds.y, bounds.width, horizontal),
        Geometry::new(bounds.x, bottom, bounds.width, horizontal),
        Geometry::new(bounds.x, bounds.y, vertical, bounds.height),
        Geometry::new(right, bounds.y, vertical, bounds.height),
    ]
}

fn fallback_cursor_geometries(location: Point<i32, Logical>) -> [Geometry; 6] {
    let x = location.x;
    let y = location.y;
    [
        Geometry::new(x, y, 2, 16),
        Geometry::new(x.saturating_add(2), y.saturating_add(2), 2, 12),
        Geometry::new(x.saturating_add(4), y.saturating_add(4), 2, 8),
        Geometry::new(x.saturating_add(6), y.saturating_add(6), 2, 4),
        Geometry::new(x.saturating_add(4), y.saturating_add(12), 2, 4),
        Geometry::new(x.saturating_add(6), y.saturating_add(16), 2, 4),
    ]
}

fn translated_cursor_geometries(
    location: Point<i32, Logical>,
    rectangles: &[(i32, i32, u32, u32)],
) -> Vec<Geometry> {
    rectangles
        .iter()
        .map(|&(x, y, width, height)| {
            Geometry::new(
                location.x.saturating_add(x),
                location.y.saturating_add(y),
                width,
                height,
            )
        })
        .collect()
}

fn named_cursor_geometries(icon: CursorIcon, location: Point<i32, Logical>) -> Vec<Geometry> {
    const CROSS: &[(i32, i32, u32, u32)] = &[(-8, -1, 18, 2), (-1, -8, 2, 18)];
    const TEXT: &[(i32, i32, u32, u32)] = &[(-1, -8, 2, 18), (-5, -8, 10, 2), (-5, 8, 10, 2)];
    const VERTICAL_TEXT: &[(i32, i32, u32, u32)] =
        &[(-8, -1, 18, 2), (-8, -5, 2, 10), (8, -5, 2, 10)];
    const HORIZONTAL_RESIZE: &[(i32, i32, u32, u32)] = &[
        (-9, -1, 19, 2),
        (-9, -1, 5, 2),
        (-7, -4, 2, 8),
        (5, -1, 5, 2),
        (6, -4, 2, 8),
    ];
    const VERTICAL_RESIZE: &[(i32, i32, u32, u32)] = &[
        (-1, -9, 2, 19),
        (-1, -9, 2, 5),
        (-4, -7, 8, 2),
        (-1, 5, 2, 5),
        (-4, 6, 8, 2),
    ];
    const DIAGONAL_DOWN: &[(i32, i32, u32, u32)] = &[
        (-7, -7, 3, 3),
        (-4, -4, 3, 3),
        (-1, -1, 3, 3),
        (2, 2, 3, 3),
        (5, 5, 3, 3),
        (-7, -7, 8, 2),
        (-7, -7, 2, 8),
        (0, 6, 8, 2),
        (6, 0, 2, 8),
    ];
    const DIAGONAL_UP: &[(i32, i32, u32, u32)] = &[
        (-7, 5, 3, 3),
        (-4, 2, 3, 3),
        (-1, -1, 3, 3),
        (2, -4, 3, 3),
        (5, -7, 3, 3),
        (-7, 6, 8, 2),
        (-7, 0, 2, 8),
        (0, -7, 8, 2),
        (6, -7, 2, 8),
    ];
    const BUSY: &[(i32, i32, u32, u32)] = &[
        (-6, -8, 12, 2),
        (-8, -6, 2, 12),
        (6, -6, 2, 12),
        (-6, 6, 12, 2),
        (-1, -6, 2, 6),
        (0, 0, 5, 2),
    ];
    const HAND: &[(i32, i32, u32, u32)] = &[
        (-5, -1, 12, 9),
        (-4, -7, 3, 8),
        (0, -9, 3, 10),
        (4, -6, 3, 7),
        (-7, 1, 3, 5),
    ];
    const FORBIDDEN: &[(i32, i32, u32, u32)] = &[
        (-6, -8, 12, 2),
        (-8, -6, 2, 12),
        (6, -6, 2, 12),
        (-6, 6, 12, 2),
        (-6, -5, 2, 2),
        (-4, -3, 2, 2),
        (-2, -1, 2, 2),
        (0, 1, 2, 2),
        (2, 3, 2, 2),
        (4, 5, 2, 2),
    ];
    const ZOOM_IN: &[(i32, i32, u32, u32)] = &[
        (-7, -7, 12, 2),
        (-7, -7, 2, 12),
        (3, -7, 2, 12),
        (-7, 3, 12, 2),
        (4, 4, 2, 2),
        (6, 6, 5, 2),
        (-4, -2, 6, 2),
        (-2, -4, 2, 6),
    ];
    const ZOOM_OUT: &[(i32, i32, u32, u32)] = &[
        (-7, -7, 12, 2),
        (-7, -7, 2, 12),
        (3, -7, 2, 12),
        (-7, 3, 12, 2),
        (4, 4, 2, 2),
        (6, 6, 5, 2),
        (-4, -2, 6, 2),
    ];

    let rectangles = match icon {
        CursorIcon::Text => TEXT,
        CursorIcon::VerticalText => VERTICAL_TEXT,
        CursorIcon::Cell | CursorIcon::Crosshair => CROSS,
        CursorIcon::Wait | CursorIcon::Progress => BUSY,
        CursorIcon::Pointer | CursorIcon::Grab | CursorIcon::Grabbing => HAND,
        CursorIcon::NoDrop | CursorIcon::NotAllowed => FORBIDDEN,
        CursorIcon::EResize
        | CursorIcon::WResize
        | CursorIcon::EwResize
        | CursorIcon::ColResize => HORIZONTAL_RESIZE,
        CursorIcon::NResize
        | CursorIcon::SResize
        | CursorIcon::NsResize
        | CursorIcon::RowResize => VERTICAL_RESIZE,
        CursorIcon::NeResize | CursorIcon::SwResize | CursorIcon::NeswResize => DIAGONAL_UP,
        CursorIcon::NwResize | CursorIcon::SeResize | CursorIcon::NwseResize => DIAGONAL_DOWN,
        CursorIcon::Move | CursorIcon::AllScroll | CursorIcon::AllResize => {
            return translated_cursor_geometries(location, CROSS)
                .into_iter()
                .chain(translated_cursor_geometries(location, &[(-5, -5, 10, 10)]))
                .collect();
        }
        CursorIcon::ZoomIn => ZOOM_IN,
        CursorIcon::ZoomOut => ZOOM_OUT,
        _ => return fallback_cursor_geometries(location).into(),
    };
    translated_cursor_geometries(location, rectangles)
}

fn load_text_renderer(configured_font: &str) -> Option<TextRenderer> {
    match TextRenderer::load(configured_font) {
        Ok(renderer) => Some(renderer),
        Err(error) => {
            warn!(%error, font = configured_font, "Wayland compositor text is unavailable");
            None
        }
    }
}

fn active_keyboard_modifiers(state: &ModifiersState) -> Vec<KeyboardModifier> {
    let mut modifiers = Vec::with_capacity(4);
    if state.ctrl {
        modifiers.push(KeyboardModifier::Control);
    }
    if state.alt {
        modifiers.push(KeyboardModifier::Alt);
    }
    if state.shift {
        modifiers.push(KeyboardModifier::Shift);
    }
    if state.logo {
        modifiers.push(KeyboardModifier::Super);
    }
    modifiers
}

fn configure_launch_environment(
    process: &mut Command,
    wayland_display: &OsString,
    activation_token: Option<&str>,
) {
    process.env("WAYLAND_DISPLAY", wayland_display);
    if let Some(token) = activation_token {
        process
            .env("XDG_ACTIVATION_TOKEN", token)
            .env("DESKTOP_STARTUP_ID", token);
    }
}

fn spawn_shell_command(command: &str, wayland_display: &OsString, activation_token: Option<&str>) {
    if command.trim().is_empty() {
        warn!("ignored empty Wayland binding command");
        return;
    }
    let mut process = Command::new("/bin/sh");
    process
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_launch_environment(&mut process, wayland_display, activation_token);
    match process.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            let command = command.to_owned();
            info!(pid, command, "started Wayland binding command");
            let _ = thread::Builder::new()
                .name(format!("nobox-wayland-child-{pid}"))
                .spawn(move || {
                    if let Err(error) = child.wait() {
                        warn!(%error, pid, command, "could not reap Wayland binding command");
                    }
                });
        }
        Err(error) => warn!(%error, command, "could not start Wayland binding command"),
    }
}

fn spawn_desktop_application(
    application: DesktopApplication,
    wayland_display: &OsString,
    activation_token: Option<&str>,
) {
    let Some((program, arguments)) = application.command.argv().split_first() else {
        warn!(
            desktop_id = application.desktop_id,
            "ignored empty desktop launch command"
        );
        return;
    };
    let mut process = if application.command.requires_terminal() {
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
    if let Some(directory) = application.command.working_directory() {
        process.current_dir(directory);
    }
    process
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_launch_environment(&mut process, wayland_display, activation_token);
    match process.spawn() {
        Ok(mut child) => {
            info!(
                pid = child.id(),
                desktop_id = application.desktop_id,
                "launched Wayland desktop application"
            );
            let name = format!("wayland-launch-{}", child.id());
            let _ = thread::Builder::new().name(name).spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => warn!(
            desktop_id = application.desktop_id,
            %error,
            "could not launch Wayland desktop application"
        ),
    }
}

struct LoopData {
    compositor: Compositor,
    display_handle: DisplayHandle,
    display_ready: bool,
    rendered_frames: usize,
    fatal_error: Option<String>,
    running: bool,
    reload_requested: bool,
    session_save_requested: bool,
    runtime_control: Option<ControlServer>,
}

impl LoopData {
    fn fail(&mut self, error: String) {
        self.fatal_error = Some(error);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BindingInput {
    modifiers: Vec<KeyboardModifier>,
    symbols: Vec<String>,
}

impl BindingInput {
    fn from_xkb(modifiers: &ModifiersState, key: KeysymHandle<'_>) -> Self {
        let mut symbols = key
            .raw_syms()
            .into_iter()
            .chain([key.modified_sym()])
            .map(xkb::keysym_get_name)
            .filter(|symbol| !symbol.is_empty())
            .collect::<Vec<_>>();
        symbols.sort_unstable();
        symbols.dedup();
        Self {
            modifiers: active_keyboard_modifiers(modifiers),
            symbols,
        }
    }

    fn matches(&self, chord: &KeyChord) -> bool {
        self.modifiers == chord.modifiers()
            && self.symbols.iter().any(|symbol| symbol == chord.symbol())
    }

    fn has_symbol(&self, expected: &str) -> bool {
        self.symbols.iter().any(|symbol| symbol == expected)
    }
}

#[derive(Clone, Debug)]
struct KeyChain {
    candidates: Vec<usize>,
    depth: usize,
    deadline: Instant,
}

enum BindingOutcome {
    Forward,
    Intercept(Vec<Action>),
}

fn resolve_configured_binding(
    config: &Config,
    key_chain: &mut Option<KeyChain>,
    input: &BindingInput,
) -> BindingOutcome {
    if key_chain
        .as_ref()
        .is_some_and(|chain| Instant::now() >= chain.deadline)
    {
        *key_chain = None;
    }
    if key_chain.is_some() && input.matches(&config.keyboard.chain_quit_key) {
        *key_chain = None;
        return BindingOutcome::Intercept(Vec::new());
    }

    let bindings = config.effective_key_bindings();
    let (depth, candidates) = key_chain.as_ref().map_or_else(
        || (0, (0..bindings.len()).collect::<Vec<_>>()),
        |chain| (chain.depth, chain.candidates.clone()),
    );
    let matching = candidates
        .into_iter()
        .filter(|index| {
            bindings
                .get(*index)
                .and_then(|binding| binding.key.chords().get(depth))
                .is_some_and(|chord| input.matches(chord))
        })
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return BindingOutcome::Forward;
    }
    if let Some(binding) = matching
        .iter()
        .filter_map(|index| bindings.get(*index))
        .find(|binding| binding.key.chords().len() == depth.saturating_add(1))
    {
        let actions = binding.actions.clone();
        *key_chain = None;
        return BindingOutcome::Intercept(actions);
    }
    *key_chain = Some(KeyChain {
        candidates: matching,
        depth: depth.saturating_add(1),
        deadline: Instant::now()
            + Duration::from_millis(u64::from(config.keyboard.chain_timeout_ms)),
    });
    BindingOutcome::Intercept(Vec::new())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActionFlow {
    Continue,
    Stop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PointerBindingTarget {
    id: Option<PolicyClientId>,
    context: MouseContext,
    resize_edge: Option<xdg_toplevel::ResizeEdge>,
}

#[derive(Clone, Copy, Debug)]
struct PointerInvocation {
    target: PointerBindingTarget,
    start: Point<f64, Logical>,
}

#[derive(Clone, Debug)]
struct MouseGesture {
    target: PointerBindingTarget,
    button: u8,
    modifiers: Vec<KeyboardModifier>,
    start: Point<f64, Logical>,
    dragged: bool,
    forwarded: bool,
}

#[derive(Clone, Debug)]
struct MouseClick {
    target: PointerBindingTarget,
    button: u8,
    modifiers: Vec<KeyboardModifier>,
    location: Point<f64, Logical>,
    time: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrameButton {
    Minimize,
    Maximize,
    Close,
}

fn geometry_contains_point(geometry: Geometry, point: Point<f64, Logical>) -> bool {
    let right = f64::from(geometry.x) + f64::from(geometry.width);
    let bottom = f64::from(geometry.y) + f64::from(geometry.height);
    point.x >= f64::from(geometry.x)
        && point.x < right
        && point.y >= f64::from(geometry.y)
        && point.y < bottom
}

fn frame_button_geometries(client: PolicyClient, config: &Config) -> Vec<(FrameButton, Geometry)> {
    if !client.policy.decorations.titlebar || client.fullscreen.is_some() {
        return Vec::new();
    }
    let titlebar_height = config.theme.titlebar_height;
    let inset = titlebar_height.min(4);
    let side = titlebar_height.saturating_sub(inset.saturating_mul(2));
    if side == 0 {
        return Vec::new();
    }
    let gap = inset.max(2).saturating_div(2);
    let titlebar_top = client
        .geometry
        .y
        .saturating_sub(i32::try_from(titlebar_height).unwrap_or(i32::MAX));
    let y = titlebar_top.saturating_add(i32::try_from(inset).unwrap_or(i32::MAX));
    let left_limit = client
        .geometry
        .x
        .saturating_add(i32::try_from(inset).unwrap_or(i32::MAX));
    let mut right = client
        .geometry
        .x
        .saturating_add(i32::try_from(client.geometry.width).unwrap_or(i32::MAX))
        .saturating_sub(i32::try_from(inset).unwrap_or(i32::MAX));
    let mut buttons = Vec::with_capacity(3);
    for (button, visible) in [
        (FrameButton::Close, client.policy.decorations.close),
        (FrameButton::Maximize, client.policy.decorations.maximize),
        (FrameButton::Minimize, client.policy.decorations.minimize),
    ] {
        if !visible {
            continue;
        }
        let x = right.saturating_sub(i32::try_from(side).unwrap_or(i32::MAX));
        if x < left_limit {
            break;
        }
        buttons.push((button, Geometry::new(x, y, side, side)));
        right = x.saturating_sub(i32::try_from(gap).unwrap_or(i32::MAX));
    }
    buttons
}

fn frame_button_glyph(button: FrameButton, geometry: Geometry) -> Vec<Geometry> {
    let inset = geometry.width.min(geometry.height).saturating_div(4).max(2);
    let inner_width = geometry.width.saturating_sub(inset.saturating_mul(2));
    let inner_height = geometry.height.saturating_sub(inset.saturating_mul(2));
    if inner_width == 0 || inner_height == 0 {
        return Vec::new();
    }
    let x = geometry
        .x
        .saturating_add(i32::try_from(inset).unwrap_or(i32::MAX));
    let y = geometry
        .y
        .saturating_add(i32::try_from(inset).unwrap_or(i32::MAX));
    let stroke = inner_width.min(inner_height).saturating_div(5).max(1);
    match button {
        FrameButton::Minimize => vec![Geometry::new(
            x,
            y.saturating_add(
                i32::try_from(inner_height.saturating_sub(stroke)).unwrap_or(i32::MAX),
            ),
            inner_width,
            stroke,
        )],
        FrameButton::Maximize => vec![
            Geometry::new(x, y, inner_width, stroke),
            Geometry::new(
                x,
                y.saturating_add(
                    i32::try_from(inner_height.saturating_sub(stroke)).unwrap_or(i32::MAX),
                ),
                inner_width,
                stroke,
            ),
            Geometry::new(x, y, stroke, inner_height),
            Geometry::new(
                x.saturating_add(
                    i32::try_from(inner_width.saturating_sub(stroke)).unwrap_or(i32::MAX),
                ),
                y,
                stroke,
                inner_height,
            ),
        ],
        FrameButton::Close => {
            let steps = 5_u32;
            let travel_x = inner_width.saturating_sub(stroke);
            let travel_y = inner_height.saturating_sub(stroke);
            (0..steps)
                .flat_map(|step| {
                    let offset_x = travel_x.saturating_mul(step) / steps.saturating_sub(1);
                    let offset_y = travel_y.saturating_mul(step) / steps.saturating_sub(1);
                    let left = x.saturating_add(i32::try_from(offset_x).unwrap_or(i32::MAX));
                    let top = y.saturating_add(i32::try_from(offset_y).unwrap_or(i32::MAX));
                    let mirrored_top = y.saturating_add(
                        i32::try_from(travel_y.saturating_sub(offset_y)).unwrap_or(i32::MAX),
                    );
                    [
                        Geometry::new(left, top, stroke, stroke),
                        Geometry::new(left, mirrored_top, stroke, stroke),
                    ]
                })
                .collect()
        }
    }
}

fn solid_geometry_element(
    geometry: Geometry,
    fill: [f32; 4],
    kind: Kind,
) -> Option<SolidColorRenderElement> {
    let width = i32::try_from(geometry.width).ok()?;
    let height = i32::try_from(geometry.height).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let buffer = SolidColorBuffer::new((width, height), fill);
    Some(SolidColorRenderElement::from_buffer(
        &buffer,
        (geometry.x, geometry.y),
        1.0,
        1.0,
        kind,
    ))
}

fn frame_context_at(
    client: PolicyClient,
    extents: DecorationExtents,
    point: Point<f64, Logical>,
) -> MouseContext {
    let outer = extents.outer_geometry(client.geometry);
    if !geometry_contains_point(outer, point) {
        return MouseContext::Frame;
    }
    let content_right = f64::from(client.geometry.x) + f64::from(client.geometry.width);
    let content_bottom = f64::from(client.geometry.y) + f64::from(client.geometry.height);
    let titlebar_height = extents.top.saturating_sub(extents.left);
    let titlebar_top = client
        .geometry
        .y
        .saturating_sub(i32::try_from(titlebar_height).unwrap_or(i32::MAX));
    let on_left = point.x < f64::from(client.geometry.x);
    let on_right = point.x >= content_right;
    let on_top = point.y < f64::from(titlebar_top);
    let on_bottom = point.y >= content_bottom;
    match (on_top, on_bottom, on_left, on_right) {
        (true, _, true, _) => MouseContext::TopLeft,
        (true, _, _, true) => MouseContext::TopRight,
        (_, true, true, _) => MouseContext::BottomLeft,
        (_, true, _, true) => MouseContext::BottomRight,
        (true, _, _, _) => MouseContext::Top,
        (_, true, _, _) => MouseContext::Bottom,
        (_, _, true, _) => MouseContext::Left,
        (_, _, _, true) => MouseContext::Right,
        _ if point.y < f64::from(client.geometry.y) => MouseContext::Titlebar,
        _ => MouseContext::Client,
    }
}

const fn context_resize_edge(context: MouseContext) -> Option<xdg_toplevel::ResizeEdge> {
    match context {
        MouseContext::Top => Some(xdg_toplevel::ResizeEdge::Top),
        MouseContext::Bottom => Some(xdg_toplevel::ResizeEdge::Bottom),
        MouseContext::Left => Some(xdg_toplevel::ResizeEdge::Left),
        MouseContext::Right => Some(xdg_toplevel::ResizeEdge::Right),
        MouseContext::TopLeft => Some(xdg_toplevel::ResizeEdge::TopLeft),
        MouseContext::TopRight => Some(xdg_toplevel::ResizeEdge::TopRight),
        MouseContext::BottomLeft => Some(xdg_toplevel::ResizeEdge::BottomLeft),
        MouseContext::BottomRight => Some(xdg_toplevel::ResizeEdge::BottomRight),
        MouseContext::Root
        | MouseContext::Desktop
        | MouseContext::Client
        | MouseContext::Frame
        | MouseContext::Titlebar
        | MouseContext::Border
        | MouseContext::Minimize
        | MouseContext::Maximize
        | MouseContext::Close => None,
    }
}

const fn configured_resize_edge(edge: ResizeEdge) -> xdg_toplevel::ResizeEdge {
    match edge {
        ResizeEdge::Top => xdg_toplevel::ResizeEdge::Top,
        ResizeEdge::Bottom => xdg_toplevel::ResizeEdge::Bottom,
        ResizeEdge::Left => xdg_toplevel::ResizeEdge::Left,
        ResizeEdge::Right => xdg_toplevel::ResizeEdge::Right,
        ResizeEdge::TopLeft => xdg_toplevel::ResizeEdge::TopLeft,
        ResizeEdge::TopRight => xdg_toplevel::ResizeEdge::TopRight,
        ResizeEdge::BottomLeft => xdg_toplevel::ResizeEdge::BottomLeft,
        ResizeEdge::BottomRight => xdg_toplevel::ResizeEdge::BottomRight,
    }
}

const fn policy_resize_edges(edge: xdg_toplevel::ResizeEdge) -> ResizeEdges {
    match edge {
        xdg_toplevel::ResizeEdge::Top => ResizeEdges::new(false, false, true, false),
        xdg_toplevel::ResizeEdge::Bottom => ResizeEdges::new(false, false, false, true),
        xdg_toplevel::ResizeEdge::Left => ResizeEdges::new(true, false, false, false),
        xdg_toplevel::ResizeEdge::Right => ResizeEdges::new(false, true, false, false),
        xdg_toplevel::ResizeEdge::TopLeft => ResizeEdges::new(true, false, true, false),
        xdg_toplevel::ResizeEdge::TopRight => ResizeEdges::new(false, true, true, false),
        xdg_toplevel::ResizeEdge::BottomLeft => ResizeEdges::new(true, false, false, true),
        xdg_toplevel::ResizeEdge::BottomRight | xdg_toplevel::ResizeEdge::None => {
            ResizeEdges::bottom_right()
        }
        _ => ResizeEdges::bottom_right(),
    }
}

const fn pointer_button_number(button: u32) -> Option<u8> {
    match button {
        0x110 => Some(1),
        0x112 => Some(2),
        0x111 => Some(3),
        _ => None,
    }
}

const MAX_PENDING_DMABUF_IMPORTS: usize = 64;
const MAX_ACTIVE_SYNCOBJ_SOURCES: usize = 256;
const MAX_PENDING_SURFACE_IMPORTS: usize = 256;
const MAX_CLIENT_SURFACES: usize = 256;
/// Maximum selection sources one client may create during a connection.
pub const MAX_CLIENT_SELECTION_SOURCES: usize = 64;
/// Maximum selection devices one client may create during a connection.
pub const MAX_CLIENT_SELECTION_DEVICES: usize = 16;
/// Maximum MIME types one selection source may advertise.
pub const MAX_SOURCE_MIME_TYPES: usize = 32;
/// Maximum byte length of one advertised selection MIME type.
pub const MAX_MIME_TYPE_BYTES: usize = 256;

/// Advertised `wp_viewporter` protocol version.
pub const VIEWPORTER_VERSION: u32 = 1;
/// Advertised `wp_fractional_scale_manager_v1` protocol version.
pub const FRACTIONAL_SCALE_VERSION: u32 = 1;
/// Advertised `wl_data_device_manager` protocol version.
pub const DATA_DEVICE_VERSION: u32 = 3;
/// Advertised `zwp_primary_selection_device_manager_v1` protocol version.
pub const PRIMARY_SELECTION_VERSION: u32 = 1;
/// Advertised `zwp_relative_pointer_manager_v1` protocol version.
pub const RELATIVE_POINTER_VERSION: u32 = 1;
/// Advertised `zwp_pointer_constraints_v1` protocol version.
pub const POINTER_CONSTRAINTS_VERSION: u32 = 1;
/// Advertised `zwp_pointer_gestures_v1` protocol version.
pub const POINTER_GESTURES_VERSION: u32 = 3;

struct CountedSurface {
    count: Arc<AtomicUsize>,
    active: AtomicBool,
}

fn reserve_bounded(counter: &AtomicUsize, limit: usize) -> bool {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < limit).then_some(current + 1)
        })
        .is_ok()
}

fn release_reservation(counter: &AtomicUsize) {
    let previous = counter.fetch_sub(1, Ordering::AcqRel);
    debug_assert!(previous > 0, "reservation count underflow");
}

struct PendingDmabufImport {
    dmabuf: Dmabuf,
    notifier: ImportNotifier,
}

struct PendingSyncobjSource {
    source: DrmSyncPointSource,
    client: Client,
}

#[derive(Clone, Copy, Default)]
struct TabletAxes {
    pressure: Option<f64>,
    distance: Option<f64>,
    tilt: Option<(f64, f64)>,
    rotation: Option<f64>,
    slider: Option<f64>,
    wheel: Option<(f64, i32)>,
}

#[derive(Clone, Copy)]
enum TabletAction {
    Axis,
    Proximity(smithay::backend::input::ProximityState),
    Tip(smithay::backend::input::TabletToolTipState),
    Button { button: u32, state: ButtonState },
}

struct TabletToolInput {
    device_id: String,
    tablet_descriptor: TabletDescriptor,
    tool_descriptor: smithay::backend::input::TabletToolDescriptor,
    location: Point<f64, Logical>,
    time: u32,
    axes: TabletAxes,
    action: TabletAction,
}

#[derive(Debug)]
struct CancelledCommitBlocker;

impl Blocker for CancelledCommitBlocker {
    fn state(&self) -> BlockerState {
        BlockerState::Cancelled
    }
}

struct Compositor {
    display_handle: DisplayHandle,
    compositor_state: CompositorState,
    shm_state: ShmState,
    dmabuf_state: DmabufState,
    dmabuf_global: Option<DmabufGlobal>,
    drm_syncobj_state: Option<DrmSyncobjState>,
    pending_dmabuf_imports: VecDeque<PendingDmabufImport>,
    pending_syncobj_sources: VecDeque<PendingSyncobjSource>,
    active_syncobj_sources: Arc<AtomicUsize>,
    pending_surface_imports: VecDeque<WlSurface>,
    data_device_state: DataDeviceState,
    primary_selection_state: PrimarySelectionState,
    clipboard_owner: Option<ClientId>,
    primary_selection_owner: Option<ClientId>,
    disconnected_client_ids: Arc<Mutex<VecDeque<ClientId>>>,
    selection_mime_counts: HashMap<ObjectId, usize>,
    _viewporter_state: ViewporterState,
    _fractional_scale_manager_state: FractionalScaleManagerState,
    _relative_pointer_manager_state: RelativePointerManagerState,
    _pointer_constraints_state: PointerConstraintsState,
    _pointer_gestures_state: PointerGesturesState,
    _cursor_shape_manager_state: CursorShapeManagerState,
    _tablet_manager_state: TabletManagerState,
    _presentation_state: PresentationState,
    keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    xdg_shell_state: XdgShellState,
    _xdg_decoration_state: XdgDecorationState,
    seat_state: SeatState<Self>,
    seat: Seat<Self>,
    _output_manager_state: OutputManagerState,
    outputs: Vec<CompositorOutput>,
    popup_manager: PopupManager,
    space: Space<Window>,
    foreign_toplevel_list_state: ForeignToplevelListState,
    xdg_activation_state: XdgActivationState,
    trusted_activation_tokens: HashSet<XdgActivationToken>,
    layer_shell_state: WlrLayerShellState,
    _workspace_global: GlobalId,
    workspace_instances: Vec<WorkspaceManagerInstance>,
    pending_workspace_activations: Vec<(ClientId, u32)>,
    config: Config,
    wayland_display: OsString,
    text_renderer: Option<TextRenderer>,
    application_catalog: ApplicationCatalog,
    clients: ClientSet,
    session_restore: SessionRestore,
    session_stacking: BTreeMap<PolicyClientId, u32>,
    windows: Vec<ManagedWindow>,
    layer_surfaces: Vec<ManagedLayerSurface>,
    tablet_devices: HashMap<String, TabletDescriptor>,
    next_client_id: u64,
    pointer_location: Point<f64, Logical>,
    nested_pointer_location: Option<Point<f64, Logical>>,
    pending_pointer_hint: Option<Point<f64, Logical>>,
    presentation_sequence: u64,
    active_shortcuts_inhibitor: Option<KeyboardShortcutsInhibitor>,
    cursor_status: CursorImageStatus,
    dnd_icon: Option<WlSurface>,
    interactive: Option<InteractiveOperation>,
    keyboard_interactive: Option<KeyboardInteractiveOperation>,
    recent_input_serials: VecDeque<RecentInputSerial>,
    key_chain: Option<KeyChain>,
    intercepted_keycodes: Vec<u32>,
    keyboard_modifiers: Vec<KeyboardModifier>,
    focus_cycle: Option<FocusCycle>,
    menu_session: Option<MenuSession>,
    mouse_gesture: Option<MouseGesture>,
    last_mouse_click: Option<MouseClick>,
    show_desktop_strict: bool,
    redraw_needed: bool,
    reload_requested: bool,
    exit_requested: bool,
    disposition: RunDisposition,
    started: Instant,
}

#[derive(Clone)]
struct CompositorOutput {
    output: Output,
    geometry: Geometry,
    primary: bool,
    global: Option<GlobalId>,
}

#[derive(Clone)]
struct ManagedLayerSurface {
    surface: DesktopLayerSurface,
    output: Output,
}

impl Compositor {
    fn new(
        display: &DisplayHandle,
        output: Output,
        size: smithay::utils::Size<i32, smithay::utils::Physical>,
        config: Config,
        wayland_display: OsString,
        restore: SessionRestore,
    ) -> Self {
        Self::new_with_outputs(
            display,
            vec![CompositorOutput {
                output,
                geometry: Geometry::new(
                    0,
                    0,
                    u32::try_from(size.w).unwrap_or(1),
                    u32::try_from(size.h).unwrap_or(1),
                ),
                primary: true,
                global: None,
            }],
            config,
            wayland_display,
            restore,
        )
    }

    fn new_with_outputs(
        display: &DisplayHandle,
        mut outputs: Vec<CompositorOutput>,
        config: Config,
        wayland_display: OsString,
        restore: SessionRestore,
    ) -> Self {
        assert!(!outputs.is_empty(), "a compositor needs one usable output");
        if !outputs.iter().any(|output| output.primary) {
            outputs[0].primary = true;
        }
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(display, "nobox");
        let _keyboard = seat
            .add_keyboard(Default::default(), 250, 25)
            .expect("the built-in keyboard configuration is valid");
        let _pointer = seat.add_pointer();
        let _touch = seat.add_touch();
        let mut space = Space::default();
        for output in &outputs {
            space.map_output(&output.output, (output.geometry.x, output.geometry.y));
        }
        let mut clients = ClientSet::default();
        let workspace_count = u32::try_from(config.workspaces.names.len()).unwrap_or(1);
        clients.set_workspace_count(workspace_count);
        clients.set_workspace_layout(configured_workspace_layout(&config));
        let initial_workspace = restore
            .current_workspace()
            .unwrap_or_else(|| config.workspaces.initial.saturating_sub(1));
        clients.switch_workspace(WorkspaceId::new(initial_workspace));
        let workspace_global = display
            .create_global::<Self, ext_workspace_manager_v1::ExtWorkspaceManagerV1, _>(1, ());
        let text_renderer = load_text_renderer(&config.theme.font);
        let application_catalog = ApplicationCatalog::discover();
        Self {
            display_handle: display.clone(),
            compositor_state: CompositorState::new::<Self>(display),
            shm_state: ShmState::new::<Self>(display, Vec::new()),
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            drm_syncobj_state: None,
            pending_dmabuf_imports: VecDeque::new(),
            pending_syncobj_sources: VecDeque::new(),
            active_syncobj_sources: Arc::new(AtomicUsize::new(0)),
            pending_surface_imports: VecDeque::new(),
            data_device_state: DataDeviceState::new::<Self>(display),
            primary_selection_state: PrimarySelectionState::new::<Self>(display),
            clipboard_owner: None,
            primary_selection_owner: None,
            disconnected_client_ids: Arc::new(Mutex::new(VecDeque::new())),
            selection_mime_counts: HashMap::new(),
            _viewporter_state: ViewporterState::new::<Self>(display),
            _fractional_scale_manager_state: FractionalScaleManagerState::new::<Self>(display),
            _relative_pointer_manager_state: RelativePointerManagerState::new::<Self>(display),
            _pointer_constraints_state: PointerConstraintsState::new::<Self>(display),
            _pointer_gestures_state: PointerGesturesState::new::<Self>(display),
            _cursor_shape_manager_state: CursorShapeManagerState::new::<Self>(display),
            _tablet_manager_state: TabletManagerState::new::<Self>(display),
            _presentation_state: PresentationState::new::<Self>(
                display,
                rustix::time::ClockId::Monotonic as u32,
            ),
            keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState::new::<Self>(display),
            xdg_shell_state: XdgShellState::new::<Self>(display),
            _xdg_decoration_state: XdgDecorationState::new::<Self>(display),
            seat_state,
            seat,
            _output_manager_state: OutputManagerState::new(),
            outputs,
            popup_manager: PopupManager::default(),
            space,
            foreign_toplevel_list_state: ForeignToplevelListState::new::<Self>(display),
            xdg_activation_state: XdgActivationState::new::<Self>(display),
            trusted_activation_tokens: HashSet::new(),
            layer_shell_state: WlrLayerShellState::new::<Self>(display),
            _workspace_global: workspace_global,
            workspace_instances: Vec::new(),
            pending_workspace_activations: Vec::new(),
            config,
            wayland_display,
            text_renderer,
            application_catalog,
            clients,
            session_restore: restore,
            session_stacking: BTreeMap::new(),
            windows: Vec::new(),
            layer_surfaces: Vec::new(),
            tablet_devices: HashMap::new(),
            next_client_id: 1,
            pointer_location: (0.0, 0.0).into(),
            nested_pointer_location: None,
            pending_pointer_hint: None,
            presentation_sequence: 0,
            active_shortcuts_inhibitor: None,
            cursor_status: CursorImageStatus::default_named(),
            dnd_icon: None,
            interactive: None,
            keyboard_interactive: None,
            recent_input_serials: VecDeque::new(),
            key_chain: None,
            intercepted_keycodes: Vec::new(),
            keyboard_modifiers: Vec::new(),
            focus_cycle: None,
            menu_session: None,
            mouse_gesture: None,
            last_mouse_click: None,
            show_desktop_strict: false,
            redraw_needed: true,
            reload_requested: false,
            exit_requested: false,
            disposition: RunDisposition::Exit,
            started: Instant::now(),
        }
    }

    fn primary_output(&self) -> &CompositorOutput {
        self.outputs
            .iter()
            .find(|output| output.primary)
            .unwrap_or(&self.outputs[0])
    }

    fn preferred_scale_for_surface(&self, surface: &WlSurface) -> f64 {
        if let Some(managed) = self.surface_window(surface)
            && let Some(client) = self.clients.get(managed.id)
        {
            return self
                .output_for_geometry(client.geometry)
                .output
                .current_scale()
                .fractional_scale();
        }
        if let Some(layer) = self.layer_surfaces.iter().find(|layer| {
            let mut found = false;
            layer.surface.with_surfaces(|candidate, _| {
                found |= candidate == surface;
            });
            found
        }) {
            return layer.output.current_scale().fractional_scale();
        }
        if matches!(&self.cursor_status, CursorImageStatus::Surface(cursor) if cursor == surface) {
            return self
                .output_for_point(self.pointer_location)
                .output
                .current_scale()
                .fractional_scale();
        }
        self.primary_output()
            .output
            .current_scale()
            .fractional_scale()
    }

    fn set_surface_preferred_scale(&self, surface: &WlSurface) {
        let scale = self.preferred_scale_for_surface(surface);
        with_states(surface, |states| {
            with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(scale);
            });
        });
    }

    fn refresh_surface_scales(&self) {
        let mut surfaces = Vec::new();
        for managed in &self.windows {
            managed.window.with_surfaces(|surface, _| {
                surfaces.push(surface.clone());
            });
        }
        for layer in &self.layer_surfaces {
            layer.surface.with_surfaces(|surface, _| {
                surfaces.push(surface.clone());
            });
        }
        if let CursorImageStatus::Surface(surface) = &self.cursor_status {
            surfaces.push(surface.clone());
        }
        if let Some(surface) = &self.dnd_icon {
            surfaces.push(surface.clone());
        }
        for surface in surfaces {
            self.set_surface_preferred_scale(&surface);
        }
    }

    fn refresh_scene(&mut self) {
        self.space.refresh();
        self.refresh_surface_scales();
    }

    fn cleanup_disconnected_selection_owners(&mut self) {
        let disconnected = {
            let mut disconnected = self.disconnected_client_ids.lock().unwrap();
            disconnected.drain(..).collect::<Vec<_>>()
        };
        if disconnected.is_empty() {
            return;
        }
        if self
            .clipboard_owner
            .as_ref()
            .is_some_and(|owner| disconnected.contains(owner))
        {
            clear_data_device_selection(&self.display_handle, &self.seat);
            self.clipboard_owner = None;
        }
        if self
            .primary_selection_owner
            .as_ref()
            .is_some_and(|owner| disconnected.contains(owner))
        {
            clear_primary_selection(&self.display_handle, &self.seat);
            self.primary_selection_owner = None;
        }
    }

    fn enable_direct_buffer_protocols(
        &mut self,
        feedback: &DmabufFeedback,
        syncobj_state: Option<DrmSyncobjState>,
    ) {
        debug_assert!(self.dmabuf_global.is_none());
        self.dmabuf_global = Some(
            self.dmabuf_state
                .create_global_with_default_feedback::<Self>(&self.display_handle, feedback),
        );
        self.drm_syncobj_state = syncobj_state;
    }

    fn take_pending_dmabuf_imports(&mut self) -> VecDeque<PendingDmabufImport> {
        std::mem::take(&mut self.pending_dmabuf_imports)
    }

    fn take_pending_syncobj_sources(&mut self) -> VecDeque<PendingSyncobjSource> {
        std::mem::take(&mut self.pending_syncobj_sources)
    }

    fn take_pending_surface_imports(&mut self) -> VecDeque<WlSurface> {
        std::mem::take(&mut self.pending_surface_imports)
    }

    fn queue_surface_import(&mut self, surface: &WlSurface) {
        if self.dmabuf_global.is_none()
            || self.pending_surface_imports.len() >= MAX_PENDING_SURFACE_IMPORTS
            || self
                .pending_surface_imports
                .iter()
                .any(|pending| pending == surface)
        {
            return;
        }
        self.pending_surface_imports.push_back(surface.clone());
    }

    fn prepare_syncobj_commit(&mut self, surface: &WlSurface) {
        let acquire_point = with_states(surface, |states| {
            let mut cached = states.cached_state.get::<DrmSyncobjCachedState>();
            cached.pending().acquire_point.clone()
        });
        let Some(acquire_point) = acquire_point else {
            return;
        };
        if self.active_syncobj_sources.load(Ordering::Relaxed) >= MAX_ACTIVE_SYNCOBJ_SOURCES {
            warn!(
                limit = MAX_ACTIVE_SYNCOBJ_SOURCES,
                "discarding explicit-sync commit after reaching the active-source bound"
            );
            add_blocker(surface, CancelledCommitBlocker);
            return;
        }
        let Some(client) = surface.client() else {
            add_blocker(surface, CancelledCommitBlocker);
            return;
        };
        match acquire_point.generate_blocker() {
            Ok((blocker, source)) => {
                self.active_syncobj_sources.fetch_add(1, Ordering::Relaxed);
                add_blocker(surface, blocker);
                self.pending_syncobj_sources
                    .push_back(PendingSyncobjSource { source, client });
            }
            Err(error) => {
                warn!(%error, "discarding explicit-sync commit whose acquire fence cannot be watched");
                add_blocker(surface, CancelledCommitBlocker);
            }
        }
    }

    fn work_area_for_output(&self, output: &CompositorOutput) -> Geometry {
        let zone = layer_map_for_output(&output.output).non_exclusive_zone();
        work_area_from_nonexclusive_zone(output.geometry, zone, self.config.margins)
    }

    fn output_set(&self) -> OutputSet {
        OutputSet::new(
            self.outputs
                .iter()
                .enumerate()
                .map(|(index, output)| PolicyOutput {
                    id: OutputId::new(u64::try_from(index).unwrap_or(u64::MAX)),
                    geometry: output.geometry,
                    primary: output.primary,
                }),
        )
    }

    fn replace_outputs(&mut self, mut outputs: Vec<CompositorOutput>) {
        assert!(!outputs.is_empty(), "a compositor needs one usable output");
        if !outputs.iter().any(|output| output.primary) {
            outputs[0].primary = true;
        }

        let replacement_primary = outputs
            .iter()
            .find(|output| output.primary)
            .unwrap_or(&outputs[0])
            .output
            .clone();
        for layer in &mut self.layer_surfaces {
            if outputs.iter().any(|output| output.output == layer.output) {
                continue;
            }
            layer_map_for_output(&layer.output).unmap_layer(&layer.surface);
            layer.output = replacement_primary.clone();
            let mut map = layer_map_for_output(&layer.output);
            if let Err(error) = map.map_layer(&layer.surface) {
                warn!(
                    ?error,
                    "could not migrate layer-shell surface after output removal"
                );
            }
            map.arrange();
        }

        for old in &self.outputs {
            if outputs.iter().any(|output| output.output == old.output) {
                continue;
            }
            self.space.unmap_output(&old.output);
            if let Some(global) = old.global.clone() {
                self.display_handle.remove_global::<Self>(global);
            }
        }
        for output in &outputs {
            self.space
                .map_output(&output.output, (output.geometry.x, output.geometry.y));
            layer_map_for_output(&output.output).arrange();
        }
        self.outputs = outputs;

        self.interactive = None;
        self.keyboard_interactive = None;
        self.mouse_gesture = None;
        let clients = self
            .windows
            .iter()
            .filter_map(|managed| self.clients.get(managed.id).copied())
            .collect::<Vec<_>>();
        for client in clients {
            let selected = self.output_for_geometry(client.geometry);
            let output_geometry = selected.geometry;
            let work_area = self.work_area_for_output(selected);
            let geometry = if client.fullscreen.is_some() {
                self.clients
                    .set_fullscreen(client.id, true, output_geometry)
            } else if let Some(maximize) = client.maximize {
                self.clients.set_maximized(
                    client.id,
                    maximize.horizontal,
                    maximize.vertical,
                    work_area,
                )
            } else if self
                .output_set()
                .overlapping_output(client.geometry)
                .is_none()
            {
                let geometry = client.geometry.clamp_position(work_area);
                self.clients
                    .set_geometry(client.id, geometry)
                    .then_some(geometry)
            } else {
                None
            };
            if let Some(geometry) = geometry
                && let Some(toplevel) = self.toplevel_for_client(client.id)
            {
                self.apply_state_geometry(&toplevel, geometry, None, false);
            }
        }
        self.pointer_location = self.clamp_point_to_outputs(self.pointer_location);
        self.refresh_scene();
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
    }

    fn output_for_point(&self, point: Point<f64, Logical>) -> &CompositorOutput {
        self.outputs
            .iter()
            .find(|output| {
                let geometry = output.geometry;
                point.x >= f64::from(geometry.x)
                    && point.y >= f64::from(geometry.y)
                    && point.x < f64::from(geometry_end_x(geometry))
                    && point.y < f64::from(geometry_end_y(geometry))
            })
            .unwrap_or_else(|| self.primary_output())
    }

    fn output_for_geometry(&self, geometry: Geometry) -> &CompositorOutput {
        let selected =
            usize::try_from(self.output_set().output_for(geometry).id.raw()).unwrap_or(0);
        self.outputs
            .get(selected)
            .unwrap_or_else(|| self.primary_output())
    }

    fn clamp_point_to_outputs(&self, point: Point<f64, Logical>) -> Point<f64, Logical> {
        self.outputs
            .iter()
            .map(|output| {
                let geometry = output.geometry;
                let maximum_x = geometry_end_x(geometry).saturating_sub(1).max(geometry.x);
                let maximum_y = geometry_end_y(geometry).saturating_sub(1).max(geometry.y);
                let clamped: Point<f64, Logical> = (
                    point.x.clamp(f64::from(geometry.x), f64::from(maximum_x)),
                    point.y.clamp(f64::from(geometry.y), f64::from(maximum_y)),
                )
                    .into();
                let delta_x = point.x - clamped.x;
                let delta_y = point.y - clamped.y;
                (delta_x.mul_add(delta_x, delta_y * delta_y), clamped)
            })
            .min_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, point)| point)
            .unwrap_or(point)
    }

    fn apply_config(&mut self, config: Config) {
        if config == self.config {
            return;
        }
        if config.theme.font != self.config.theme.font {
            self.text_renderer = load_text_renderer(&config.theme.font);
        }
        self.clients
            .set_workspace_count(u32::try_from(config.workspaces.names.len()).unwrap_or(1));
        self.clients
            .set_workspace_layout(configured_workspace_layout(&config));
        self.config = config;
        self.application_catalog = ApplicationCatalog::discover();
        self.key_chain = None;
        self.focus_cycle = None;
        self.menu_session = None;
        self.mouse_gesture = None;
        self.last_mouse_click = None;
        self.sync_workspace_protocol();
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
    }

    fn session_snapshot(&self) -> SessionSnapshot {
        let clients = self
            .clients
            .stacking()
            .enumerate()
            .filter_map(|(stacking_index, id)| {
                let client = self.clients.get(id).copied()?;
                let managed = self.windows.iter().find(|managed| managed.id == id)?;
                let identity = SessionIdentity::native(
                    &managed.app_id,
                    &managed.title,
                    session_role(client.policy.role),
                    "wayland",
                )?;
                let geometry = client.unmanaged_geometry();
                Some(SessionClient {
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
                    layer: session_layer(client.layer),
                    decoration_override: session_decoration(client.decoration_override),
                    focused: self.clients.focused() == Some(id),
                    stacking_index: u32::try_from(stacking_index).unwrap_or(u32::MAX),
                })
            })
            .collect();
        SessionSnapshot::new(self.clients.current_workspace().index(), clients)
    }

    fn restore_session_stacking(&mut self) {
        let mut order = self.clients.stacking().collect::<Vec<_>>();
        order.sort_by_key(|id| {
            self.session_stacking
                .get(id)
                .map_or((1, u32::MAX), |index| (0, *index))
        });
        self.clients.sync_stacking(order);
    }

    fn workspace_name(&self, index: u32) -> String {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.config.workspaces.names.get(index))
            .cloned()
            .unwrap_or_else(|| index.saturating_add(1).to_string())
    }

    fn send_workspace_properties(&self, workspace: &WorkspaceResource) {
        workspace.handle.id(format!(
            "nobox-workspace-{}",
            workspace.index.saturating_add(1)
        ));
        workspace.handle.name(self.workspace_name(workspace.index));
        workspace
            .handle
            .capabilities(ext_workspace_handle_v1::WorkspaceCapabilities::Activate);
        let state = if workspace.index == self.clients.current_workspace().index() {
            ext_workspace_handle_v1::State::Active
        } else {
            ext_workspace_handle_v1::State::empty()
        };
        workspace.handle.state(state);
    }

    fn sync_workspace_protocol(&mut self) {
        let count = u32::try_from(self.config.workspaces.names.len()).unwrap_or(1);
        let display = self.display_handle.clone();
        for instance_index in 0..self.workspace_instances.len() {
            let manager = self.workspace_instances[instance_index].manager.clone();
            let group = self.workspace_instances[instance_index].group.clone();
            while self.workspace_instances[instance_index].workspaces.len()
                > usize::try_from(count).unwrap_or(usize::MAX)
            {
                if let Some(workspace) = self.workspace_instances[instance_index].workspaces.pop() {
                    group.workspace_leave(&workspace.handle);
                    workspace.handle.removed();
                }
            }
            if let Ok(client) = display.get_client(manager.id()) {
                let start =
                    u32::try_from(self.workspace_instances[instance_index].workspaces.len())
                        .unwrap_or(count);
                for index in start..count {
                    let Ok(handle) = client
                        .create_resource::<ext_workspace_handle_v1::ExtWorkspaceHandleV1, _, Self>(
                            &display,
                            manager.version(),
                            WorkspaceResourceData { index },
                        )
                    else {
                        break;
                    };
                    manager.workspace(&handle);
                    let workspace = WorkspaceResource { handle, index };
                    self.send_workspace_properties(&workspace);
                    group.workspace_enter(&workspace.handle);
                    self.workspace_instances[instance_index]
                        .workspaces
                        .push(workspace);
                }
            }
            for workspace in &self.workspace_instances[instance_index].workspaces {
                self.send_workspace_properties(workspace);
            }
            manager.done();
        }
    }

    fn work_area(&self) -> Geometry {
        self.work_area_for_output(self.primary_output())
    }

    fn toplevel_metadata(
        &self,
        index: usize,
    ) -> (String, String, ClientRole, Option<PolicyClientId>, bool) {
        let managed = &self.windows[index];
        let Some(toplevel) = managed.window.toplevel() else {
            return (
                String::new(),
                String::new(),
                ClientRole::Normal,
                None,
                false,
            );
        };
        let (title, app_id, modal) = with_states(toplevel.wl_surface(), |states| {
            let attributes = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .expect("xdg toplevel surfaces carry role attributes")
                .lock()
                .unwrap();
            (
                bounded_protocol_text(attributes.title.as_deref(), 1024),
                bounded_protocol_text(attributes.app_id.as_deref(), 256),
                attributes.modal,
            )
        });
        let parent = toplevel
            .parent()
            .and_then(|surface| self.surface_window(&surface).map(|window| window.id));
        let role = if parent.is_some() {
            ClientRole::Dialog
        } else {
            ClientRole::Normal
        };
        (title, app_id, role, parent, modal)
    }

    fn surface_window(&self, surface: &WlSurface) -> Option<&ManagedWindow> {
        self.windows.iter().find(|managed| {
            let mut found = false;
            managed.window.with_surfaces(|candidate, _| {
                found |= candidate == surface;
            });
            found
        })
    }

    fn layer_surface_at(
        &self,
        location: Point<f64, Logical>,
        layers: &[WlrLayer],
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.output_for_point(location);
        let map = layer_map_for_output(&output.output);
        let output_origin: Point<f64, Logical> =
            (f64::from(output.geometry.x), f64::from(output.geometry.y)).into();
        let local_location = location - output_origin;
        layers.iter().find_map(|layer_kind| {
            let layer = map.layer_under(*layer_kind, local_location)?;
            let geometry = map.layer_geometry(layer)?;
            layer
                .surface_under(
                    local_location - geometry.loc.to_f64(),
                    WindowSurfaceType::ALL,
                )
                .map(|(surface, surface_location)| {
                    let output_origin: Point<i32, Logical> =
                        (output.geometry.x, output.geometry.y).into();
                    (
                        surface,
                        (geometry.loc + surface_location + output_origin).to_f64(),
                    )
                })
        })
    }

    fn layer_for_surface(&self, surface: &WlSurface) -> Option<DesktopLayerSurface> {
        self.layer_surfaces
            .iter()
            .find(|layer| layer.surface.wl_surface() == surface)
            .map(|layer| layer.surface.clone())
    }

    fn exclusive_keyboard_layer(&self) -> Option<WlSurface> {
        self.outputs.iter().find_map(|output| {
            let map = layer_map_for_output(&output.output);
            [WlrLayer::Overlay, WlrLayer::Top]
                .into_iter()
                .find_map(|layer_kind| {
                    map.layers_on(layer_kind)
                        .rev()
                        .find(|layer| {
                            layer.cached_state().keyboard_interactivity
                                == KeyboardInteractivity::Exclusive
                        })
                        .map(|layer| layer.wl_surface().clone())
                })
        })
    }

    fn resize_output(&mut self, size: smithay::utils::Size<i32, Physical>) {
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        let primary_index = self
            .outputs
            .iter()
            .position(|output| output.primary)
            .unwrap_or(0);
        self.outputs[primary_index].geometry.width = u32::try_from(size.w).unwrap_or(1);
        self.outputs[primary_index].geometry.height = u32::try_from(size.h).unwrap_or(1);
        let mode = OutputMode {
            size,
            refresh: 60_000,
        };
        self.outputs[primary_index]
            .output
            .change_current_state(Some(mode), None, None, None);
        self.outputs[primary_index].output.set_preferred(mode);
        layer_map_for_output(&self.outputs[primary_index].output).arrange();
        for managed in &self.windows {
            if let Some(toplevel) = managed.window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.bounds = Some((size.w, size.h).into());
                });
                toplevel.send_pending_configure();
            }
        }
        self.redraw_needed = true;
    }

    fn finish_frame_callbacks(&mut self) {
        let elapsed = u32::try_from(self.started.elapsed().as_millis()).unwrap_or(u32::MAX);
        let output = self.primary_output().output.clone();
        let presented_at = monotonic_time();
        let refresh = presentation_refresh(&output);
        self.presentation_sequence = self.presentation_sequence.saturating_add(1);
        let sequence = self.presentation_sequence;
        for managed in &self.windows {
            if let Some(surface) = managed.window.wl_surface() {
                send_surface_callbacks(
                    &surface,
                    elapsed,
                    &output,
                    presented_at,
                    refresh,
                    sequence,
                    wp_presentation_feedback::Kind::empty(),
                );
                let popups = PopupManager::popups_for_surface(&surface)
                    .map(|(popup, _)| popup.wl_surface().clone())
                    .collect::<Vec<_>>();
                for popup in popups {
                    send_surface_callbacks(
                        &popup,
                        elapsed,
                        &output,
                        presented_at,
                        refresh,
                        sequence,
                        wp_presentation_feedback::Kind::empty(),
                    );
                }
            }
        }
        for layer in &self.layer_surfaces {
            send_surface_callbacks(
                layer.surface.wl_surface(),
                elapsed,
                &output,
                presented_at,
                refresh,
                sequence,
                wp_presentation_feedback::Kind::empty(),
            );
            for (popup, _) in PopupManager::popups_for_surface(layer.surface.wl_surface()) {
                send_surface_callbacks(
                    popup.wl_surface(),
                    elapsed,
                    &output,
                    presented_at,
                    refresh,
                    sequence,
                    wp_presentation_feedback::Kind::empty(),
                );
            }
        }
        if let Some(surface) = &self.dnd_icon {
            send_surface_callbacks(
                surface,
                elapsed,
                &output,
                presented_at,
                refresh,
                sequence,
                wp_presentation_feedback::Kind::empty(),
            );
        }
        self.redraw_needed = false;
    }

    fn finish_frame_callbacks_for_output(&mut self, output: &Output) {
        let elapsed = u32::try_from(self.started.elapsed().as_millis()).unwrap_or(u32::MAX);
        let presented_at = monotonic_time();
        let refresh = presentation_refresh(output);
        self.presentation_sequence = self.presentation_sequence.saturating_add(1);
        let sequence = self.presentation_sequence;
        for managed in &self.windows {
            let Some(client) = self.clients.get(managed.id).copied() else {
                continue;
            };
            if self.output_for_geometry(client.geometry).output != *output {
                continue;
            }
            if let Some(surface) = managed.window.wl_surface() {
                send_surface_callbacks(
                    &surface,
                    elapsed,
                    output,
                    presented_at,
                    refresh,
                    sequence,
                    wp_presentation_feedback::Kind::Vsync,
                );
                let popups = PopupManager::popups_for_surface(&surface)
                    .map(|(popup, _)| popup.wl_surface().clone())
                    .collect::<Vec<_>>();
                for popup in popups {
                    send_surface_callbacks(
                        &popup,
                        elapsed,
                        output,
                        presented_at,
                        refresh,
                        sequence,
                        wp_presentation_feedback::Kind::Vsync,
                    );
                }
            }
        }
        for layer in self
            .layer_surfaces
            .iter()
            .filter(|layer| layer.output == *output)
        {
            send_surface_callbacks(
                layer.surface.wl_surface(),
                elapsed,
                output,
                presented_at,
                refresh,
                sequence,
                wp_presentation_feedback::Kind::Vsync,
            );
            for (popup, _) in PopupManager::popups_for_surface(layer.surface.wl_surface()) {
                send_surface_callbacks(
                    popup.wl_surface(),
                    elapsed,
                    output,
                    presented_at,
                    refresh,
                    sequence,
                    wp_presentation_feedback::Kind::Vsync,
                );
            }
        }
        if self
            .dnd_icon
            .as_ref()
            .is_some_and(|_| self.output_for_point(self.pointer_location).output == *output)
            && let Some(surface) = &self.dnd_icon
        {
            send_surface_callbacks(
                surface,
                elapsed,
                output,
                presented_at,
                refresh,
                sequence,
                wp_presentation_feedback::Kind::Vsync,
            );
        }
    }

    fn commit_layer_surface(&mut self, surface: &WlSurface) {
        let Some(layer) = self
            .layer_surfaces
            .iter()
            .find(|layer| layer.surface.wl_surface() == surface)
            .cloned()
        else {
            return;
        };
        let (configured, initial_configure_sent) = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .map(|data| {
                    let data = data.lock().unwrap();
                    (data.configured, data.initial_configure_sent)
                })
                .unwrap_or_default()
        });
        let has_buffer =
            with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
        let mut map = layer_map_for_output(&layer.output);
        if !initial_configure_sent {
            if let Err(error) = map.map_layer(&layer.surface) {
                warn!(?error, "could not map layer-shell surface");
                return;
            }
            map.arrange();
            layer.surface.layer_surface().send_configure();
        } else if configured && has_buffer {
            if let Err(error) = map.map_layer(&layer.surface) {
                warn!(?error, "could not map configured layer-shell surface");
            }
            map.arrange();
        } else if configured {
            map.unmap_layer(&layer.surface);
        }
        self.redraw_needed = true;
    }

    fn window_for_toplevel(&self, surface: &ToplevelSurface) -> Option<&ManagedWindow> {
        self.windows
            .iter()
            .find(|managed| managed.window.wl_surface().as_deref() == Some(surface.wl_surface()))
    }

    fn valid_interactive_request(&self, surface: &ToplevelSurface, serial: Serial) -> bool {
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        let Some(focused) = pointer.current_focus() else {
            return false;
        };
        let Some(managed) = self.window_for_toplevel(surface) else {
            return false;
        };
        let mut belongs_to_window = false;
        managed.window.with_surfaces(|candidate, _| {
            belongs_to_window |= candidate == &focused;
        });
        pointer.has_grab(serial) && belongs_to_window
    }

    fn map_toplevel_if_ready(&mut self, surface: &WlSurface) {
        let Some(index) = self
            .windows
            .iter()
            .position(|managed| managed.window.wl_surface().as_deref() == Some(surface))
        else {
            return;
        };
        let has_buffer =
            with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
        let id = self.windows[index].id;
        let window = self.windows[index].window.clone();
        window.on_commit();
        if !has_buffer {
            if self
                .keyboard_interactive
                .is_some_and(|operation| operation.id == id)
            {
                self.keyboard_interactive = None;
            }
            self.space.unmap_elem(&window);
            let _ = self.clients.unmanage(id);
            self.session_stacking.remove(&id);
            if let Some(handle) = self.windows[index].foreign_toplevel.take() {
                self.foreign_toplevel_list_state.remove_toplevel(&handle);
            }
            self.redraw_needed = true;
            return;
        }
        if window
            .toplevel()
            .is_some_and(|toplevel| !toplevel.ensure_configured())
        {
            return;
        }
        let size_hints = with_states(surface, |states| {
            let mut cached = states.cached_state.get::<XdgSurfaceCachedState>();
            let state = cached.current();
            SizeHints {
                minimum: (state.min_size.w > 0 && state.min_size.h > 0).then(|| {
                    Size::new(
                        u32::try_from(state.min_size.w).unwrap_or(1),
                        u32::try_from(state.min_size.h).unwrap_or(1),
                    )
                }),
                maximum: (state.max_size.w > 0 && state.max_size.h > 0).then(|| {
                    Size::new(
                        u32::try_from(state.max_size.w).unwrap_or(u32::MAX),
                        u32::try_from(state.max_size.h).unwrap_or(u32::MAX),
                    )
                }),
                ..SizeHints::default()
            }
        });

        let geometry = window.geometry();
        let width = u32::try_from(geometry.size.w.max(1)).unwrap_or(1);
        let height = u32::try_from(geometry.size.h.max(1)).unwrap_or(1);
        let (title, app_id, role, parent, modal) = self.toplevel_metadata(index);
        self.windows[index].title.clone_from(&title);
        self.windows[index].app_id.clone_from(&app_id);
        if let Some(handle) = &self.windows[index].foreign_toplevel {
            handle.send_title(&title);
            handle.send_app_id(&app_id);
            handle.send_done();
        }
        if !self.clients.contains(id) {
            let application = self.config.application_settings(ApplicationIdentity {
                name: &app_id,
                class: &app_id,
                group_name: "",
                group_class: "",
                role: "",
                title: &title,
                kind: match role {
                    ClientRole::Dialog => ApplicationKind::Dialog,
                    _ => ApplicationKind::Normal,
                },
            });
            let restored = SessionIdentity::native(&app_id, &title, session_role(role), "wayland")
                .and_then(|identity| self.session_restore.take_match(&identity));
            if self.clients.showing_desktop()
                && !self.show_desktop_strict
                && role.occupies_placement_space()
            {
                self.clients.set_showing_desktop(false);
            }
            let work_area = self.work_area();
            let policy = ClientPolicy::for_role(role);
            let decoration_override = restored.as_ref().map_or_else(
                || match application.decorated {
                    Some(true) => DecorationOverride::Decorated,
                    Some(false) => DecorationOverride::Undecorated,
                    None => DecorationOverride::Default,
                },
                |saved| restored_decoration(saved.decoration_override),
            );
            let decorated = policy
                .with_decoration_override(decoration_override)
                .decorations
                .is_present();
            let border = if decorated {
                self.config.theme.border_width
            } else {
                0
            };
            let top = if decorated {
                self.config.theme.titlebar_height.saturating_add(border)
            } else {
                0
            };
            let requested_size = restored.as_ref().map_or_else(
                || {
                    Size::new(
                        requested_application_dimension(
                            application.size.and_then(|size| size.width),
                            application
                                .size
                                .map_or(SizeBasis::Content, |size| size.width_basis),
                            work_area.width,
                            width,
                            border.saturating_mul(2),
                        ),
                        requested_application_dimension(
                            application.size.and_then(|size| size.height),
                            application
                                .size
                                .map_or(SizeBasis::Content, |size| size.height_basis),
                            work_area.height,
                            height,
                            top.saturating_add(border),
                        ),
                    )
                },
                |saved| Size::new(saved.width, saved.height),
            );
            let requested_size = size_hints.constrain(requested_size);
            let outer_size = Size::new(
                requested_size
                    .width
                    .saturating_add(border.saturating_mul(2)),
                requested_size
                    .height
                    .saturating_add(top)
                    .saturating_add(border),
            );
            let obstacles = self
                .clients
                .management_order()
                .filter(|candidate| self.clients.is_visible(*candidate))
                .filter_map(|candidate| self.clients.get(candidate).map(|client| client.geometry))
                .collect::<Vec<_>>();
            let placed = smart_placement(
                outer_size,
                work_area,
                &obstacles,
                self.config.placement.center_free_space,
            );
            let mut placed = Geometry::new(
                placed
                    .x
                    .saturating_add(i32::try_from(border).unwrap_or(i32::MAX)),
                placed
                    .y
                    .saturating_add(i32::try_from(top).unwrap_or(i32::MAX)),
                requested_size.width,
                requested_size.height,
            );
            if let Some(saved) = &restored {
                placed.x = saved.x;
                placed.y = saved.y;
            } else if let Some(position) = application.position {
                let outer = Geometry::new(
                    placed
                        .x
                        .saturating_sub(i32::try_from(border).unwrap_or(i32::MAX)),
                    placed
                        .y
                        .saturating_sub(i32::try_from(top).unwrap_or(i32::MAX)),
                    outer_size.width,
                    outer_size.height,
                );
                let x = position.x.map_or(outer.x, |axis| {
                    placed_application_axis(axis, work_area.x, work_area.width, outer.width)
                });
                let y = position.y.map_or(outer.y, |axis| {
                    placed_application_axis(axis, work_area.y, work_area.height, outer.height)
                });
                placed.x = x.saturating_add(i32::try_from(border).unwrap_or(i32::MAX));
                placed.y = y.saturating_add(i32::try_from(top).unwrap_or(i32::MAX));
            }
            let presentation = ClientPresentation {
                skip_taskbar: restored.as_ref().map_or_else(
                    || application.skip_taskbar.unwrap_or(false),
                    |saved| saved.skip_taskbar,
                ),
                skip_pager: restored.as_ref().map_or_else(
                    || application.skip_pager.unwrap_or(false),
                    |saved| saved.skip_pager,
                ),
                urgent: false,
            };
            let workspace = parent
                .and_then(|parent| self.clients.get(parent).map(|client| client.workspace))
                .or_else(|| {
                    application.workspace.map(|workspace| match workspace {
                        ApplicationWorkspace::All => WorkspaceAssignment::All,
                        ApplicationWorkspace::Index(workspace) => WorkspaceAssignment::Workspace(
                            WorkspaceId::new(workspace.get().saturating_sub(1)),
                        ),
                    })
                })
                .unwrap_or(WorkspaceAssignment::Workspace(
                    self.clients.current_workspace(),
                ));
            let workspace = restored.as_ref().map_or(workspace, |saved| {
                saved
                    .workspace
                    .map_or(WorkspaceAssignment::All, |workspace| {
                        WorkspaceAssignment::Workspace(WorkspaceId::new(
                            workspace.min(self.clients.workspace_count().saturating_sub(1)),
                        ))
                    })
            });
            let iconic = restored.as_ref().map_or_else(
                || application.minimized.unwrap_or(false),
                |saved| saved.iconic,
            );
            let shaded = restored
                .as_ref()
                .map_or_else(|| application.shaded.unwrap_or(false), |saved| saved.shaded);
            let layer = restored.as_ref().map_or_else(
                || {
                    application
                        .layer
                        .map_or(ClientLayer::Normal, application_layer)
                },
                |saved| restored_layer(saved.layer),
            );
            let _ = self.clients.manage(PolicyClient {
                id,
                geometry: placed,
                size_hints,
                gravity: Gravity::default(),
                policy,
                natural_decorations: policy.decorations,
                decoration_override,
                presentation,
                transient_for: parent.map(TransientTarget::Client),
                group: None,
                modal: modal && parent.is_some(),
                iconic,
                shaded,
                workspace,
                layer,
                maximize: None,
                fullscreen: None,
                output_coverage: None,
            });
            if (requested_size.width != width || requested_size.height != height)
                && let Some(toplevel) = window.toplevel().cloned()
            {
                self.apply_state_geometry(&toplevel, placed, None, false);
            }
            self.windows[index].foreign_toplevel = Some(
                self.foreign_toplevel_list_state
                    .new_toplevel::<Self>(title, app_id),
            );
            if let Some(saved) = &restored {
                self.session_stacking.insert(id, saved.stacking_index);
                self.restore_session_stacking();
            }
            let focus_new = restored.as_ref().is_some_and(|saved| saved.focused)
                || application.focus.unwrap_or(self.config.focus.focus_new);
            if !iconic && focus_new {
                let _ = self.clients.focus(id);
                if self.config.focus.raise_on_focus {
                    let _ = self.clients.raise(id);
                }
            }
            let maximized = restored.as_ref().map_or_else(
                || application.maximized.map(|maximized| maximized.axes()),
                |saved| Some((saved.maximized_horizontal, saved.maximized_vertical)),
            );
            if let Some((horizontal, vertical)) = maximized
                && (horizontal || vertical)
                && let Some(geometry) = self
                    .clients
                    .set_maximized(id, horizontal, vertical, work_area)
                && let Some(toplevel) = window.toplevel().cloned()
            {
                self.apply_state_geometry(
                    &toplevel,
                    geometry,
                    Some(xdg_toplevel::State::Maximized),
                    horizontal && vertical,
                );
            }
            let fullscreen = restored.as_ref().map_or_else(
                || application.fullscreen.unwrap_or(false),
                |saved| saved.fullscreen,
            );
            if fullscreen
                && let Some(geometry) =
                    self.clients
                        .set_fullscreen(id, true, self.primary_output().geometry)
                && let Some(toplevel) = window.toplevel().cloned()
            {
                self.apply_state_geometry(
                    &toplevel,
                    geometry,
                    Some(xdg_toplevel::State::Fullscreen),
                    true,
                );
            }
            self.sync_focus_and_stacking();
        } else if let Some(current) = self.clients.get(id).copied() {
            let _ = self.clients.set_size_hints(id, size_hints);
            let _ = self.clients.set_relationships(
                id,
                parent.map(TransientTarget::Client),
                None,
                modal && parent.is_some(),
            );
            let _ = self.clients.set_geometry(
                id,
                Geometry::new(current.geometry.x, current.geometry.y, width, height),
            );
            self.space
                .map_element(window, (current.geometry.x, current.geometry.y), false);
        }
        self.redraw_needed = true;
    }

    fn sync_focus_and_stacking(&mut self) {
        let focused = self.clients.focused();
        if let Some(focused) = focused
            && let Some(mut presentation) =
                self.clients.get(focused).map(|client| client.presentation)
            && presentation.urgent
        {
            presentation.urgent = false;
            let _ = self.clients.set_presentation(focused, presentation);
        }
        for managed in &self.windows {
            managed.window.set_activated(focused == Some(managed.id));
        }
        let ordered = self.clients.stacking().collect::<Vec<_>>();
        for id in ordered {
            let Some(managed) = self.windows.iter().find(|window| window.id == id) else {
                continue;
            };
            let Some(client) = self.clients.get(id) else {
                continue;
            };
            if self.clients.is_visible(id) && !client.iconic {
                self.space.map_element(
                    managed.window.clone(),
                    (client.geometry.x, client.geometry.y),
                    focused == Some(id),
                );
            } else {
                self.space.unmap_elem(&managed.window);
            }
        }
        let keyboard_focus = self.exclusive_keyboard_layer().or_else(|| {
            focused.and_then(|id| {
                self.windows
                    .iter()
                    .find(|window| window.id == id)
                    .and_then(|window| {
                        window
                            .window
                            .wl_surface()
                            .map(|surface| surface.into_owned())
                    })
            })
        });
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, keyboard_focus, SERIAL_COUNTER.next_serial());
        }
        for managed in &self.windows {
            if managed.window.set_activated(focused == Some(managed.id)) {
                if let Some(toplevel) = managed.window.toplevel() {
                    toplevel.send_pending_configure();
                }
            }
        }
    }

    fn pointer_binding_target_at(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<PointerBindingTarget> {
        if self
            .layer_surface_at(
                location,
                &[
                    WlrLayer::Overlay,
                    WlrLayer::Top,
                    WlrLayer::Bottom,
                    WlrLayer::Background,
                ],
            )
            .is_some()
        {
            return None;
        }
        let stacking = self.clients.stacking().collect::<Vec<_>>();
        for id in stacking.into_iter().rev() {
            let Some(client) = self.clients.get(id).copied() else {
                continue;
            };
            if !self.clients.is_visible(id) || client.iconic {
                continue;
            }
            let extents = self.client_decoration_extents(client);
            if !geometry_contains_point(extents.outer_geometry(client.geometry), location) {
                continue;
            }
            for (button, geometry) in frame_button_geometries(client, &self.config) {
                if geometry_contains_point(geometry, location) {
                    let context = match button {
                        FrameButton::Minimize => MouseContext::Minimize,
                        FrameButton::Maximize => MouseContext::Maximize,
                        FrameButton::Close => MouseContext::Close,
                    };
                    return Some(PointerBindingTarget {
                        id: Some(id),
                        context,
                        resize_edge: None,
                    });
                }
            }
            let mut context = frame_context_at(client, extents, location);
            if context == MouseContext::Client && client.policy.role == ClientRole::Desktop {
                context = MouseContext::Desktop;
            }
            return Some(PointerBindingTarget {
                id: Some(id),
                context,
                resize_edge: context_resize_edge(context),
            });
        }
        Some(PointerBindingTarget {
            id: None,
            context: MouseContext::Root,
            resize_edge: None,
        })
    }

    fn mouse_binding_actions(
        &self,
        target: PointerBindingTarget,
        button: u8,
        modifiers: &[KeyboardModifier],
        trigger: MouseTrigger,
    ) -> Option<Vec<Action>> {
        let bindings = self.config.mouse.effective_bindings();
        mouse_context_chain(target.context)
            .iter()
            .find_map(|context| {
                bindings.iter().find(|binding| {
                    binding.context == *context
                        && binding.button.button().number() == button
                        && binding.button.modifiers() == modifiers
                        && binding.trigger == trigger
                })
            })
            .map(|binding| binding.actions.clone())
    }

    fn has_mouse_binding(
        &self,
        target: PointerBindingTarget,
        button: u8,
        modifiers: &[KeyboardModifier],
    ) -> bool {
        let bindings = self.config.mouse.effective_bindings();
        mouse_context_chain(target.context).iter().any(|context| {
            bindings.iter().any(|binding| {
                binding.context == *context
                    && binding.button.button().number() == button
                    && binding.button.modifiers() == modifiers
            })
        })
    }

    fn dispatch_mouse_binding(
        &mut self,
        invocation: PointerInvocation,
        button: u8,
        modifiers: &[KeyboardModifier],
        trigger: MouseTrigger,
        time: u32,
    ) -> bool {
        let Some(actions) =
            self.mouse_binding_actions(invocation.target, button, modifiers, trigger)
        else {
            return false;
        };
        let _ = self.run_actions_with_pointer(actions, invocation, time);
        true
    }

    fn update_mouse_gesture(&mut self, location: Point<f64, Logical>, time: u32) {
        let Some(mut gesture) = self.mouse_gesture.take() else {
            return;
        };
        let dx = (location.x - gesture.start.x).abs();
        let dy = (location.y - gesture.start.y).abs();
        if gesture.dragged
            || (dx < f64::from(self.config.mouse.drag_threshold)
                && dy < f64::from(self.config.mouse.drag_threshold))
        {
            self.mouse_gesture = Some(gesture);
            return;
        }
        gesture.dragged = true;
        let invocation = PointerInvocation {
            target: gesture.target,
            start: gesture.start,
        };
        let button = gesture.button;
        let modifiers = gesture.modifiers.clone();
        self.mouse_gesture = Some(gesture);
        let _ =
            self.dispatch_mouse_binding(invocation, button, &modifiers, MouseTrigger::Drag, time);
    }

    fn finish_mouse_click(
        &mut self,
        current: MouseClick,
        invocation: PointerInvocation,
        time: u32,
    ) {
        let _ = self.dispatch_mouse_binding(
            invocation,
            current.button,
            &current.modifiers,
            MouseTrigger::Click,
            time,
        );
        let double_click = self.last_mouse_click.take().is_some_and(|previous| {
            previous.target == current.target
                && previous.button == current.button
                && previous.modifiers == current.modifiers
                && current.time.duration_since(previous.time)
                    <= Duration::from_millis(u64::from(self.config.mouse.double_click_ms))
                && (current.location.x - previous.location.x).abs() < 8.0
                && (current.location.y - previous.location.y).abs() < 8.0
        });
        if double_click {
            let _ = self.dispatch_mouse_binding(
                invocation,
                current.button,
                &current.modifiers,
                MouseTrigger::DoubleClick,
                time,
            );
        } else {
            self.last_mouse_click = Some(current);
        }
    }

    fn pointer_focus_at(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.layer_surface_at(location, &[WlrLayer::Overlay, WlrLayer::Top])
            .or_else(|| {
                self.space
                    .element_under(location)
                    .and_then(|(window, window_location)| {
                        window
                            .surface_under(
                                location - window_location.to_f64(),
                                WindowSurfaceType::ALL,
                            )
                            .map(|(surface, surface_location)| {
                                (surface, (window_location + surface_location).to_f64())
                            })
                    })
            })
            .or_else(|| self.layer_surface_at(location, &[WlrLayer::Bottom, WlrLayer::Background]))
    }

    fn constrained_pointer_location(&self, desired: Point<f64, Logical>) -> Point<f64, Logical> {
        let Some(pointer) = self.seat.get_pointer() else {
            return desired;
        };
        let Some(surface) = pointer.current_focus() else {
            return desired;
        };
        let Some((focused_surface, origin)) = self.pointer_focus_at(self.pointer_location) else {
            return desired;
        };
        if focused_surface != surface {
            return desired;
        }
        with_pointer_constraint(&surface, &pointer, |constraint| {
            let Some(constraint) = constraint.filter(|constraint| constraint.is_active()) else {
                return desired;
            };
            match &*constraint {
                PointerConstraint::Locked(_) => self.pointer_location,
                PointerConstraint::Confined(confined) => {
                    let inside = confined.region().map_or_else(
                        || {
                            self.pointer_focus_at(desired)
                                .is_some_and(|(candidate, _)| candidate == surface)
                        },
                        |region| {
                            let local = desired - origin;
                            region.contains((local.x.floor() as i32, local.y.floor() as i32))
                        },
                    );
                    if inside {
                        desired
                    } else {
                        self.pointer_location
                    }
                }
            }
        })
    }

    fn apply_pending_pointer_hint(&mut self) -> bool {
        let Some(hint) = self.pending_pointer_hint else {
            return false;
        };
        let constraint_active = self.seat.get_pointer().is_some_and(|pointer| {
            pointer.current_focus().is_some_and(|surface| {
                with_pointer_constraint(&surface, &pointer, |constraint| {
                    constraint.is_some_and(|constraint| constraint.is_active())
                })
            })
        });
        if !constraint_active {
            self.pointer_location = self.clamp_point_to_outputs(hint);
            self.pending_pointer_hint = None;
            return true;
        }
        false
    }

    fn cache_pointer_constraint_hint(&mut self, surface: &WlSurface) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if pointer.current_focus().as_ref() != Some(surface) {
            return;
        }
        let hint = with_pointer_constraint(surface, &pointer, |constraint| {
            constraint.and_then(|constraint| match &*constraint {
                PointerConstraint::Locked(locked) if constraint.is_active() => {
                    locked.cursor_position_hint()
                }
                _ => None,
            })
        });
        if let Some(hint) = hint
            && let Some((_focused, origin)) = self.pointer_focus_at(self.pointer_location)
        {
            self.pending_pointer_hint = Some(origin + hint);
        }
    }

    fn pointer_motion(&mut self, x: f64, y: f64, time: u32) {
        self.apply_pending_pointer_hint();
        let location = self.constrained_pointer_location((x, y).into());
        self.pointer_location = location;
        if self.menu_session.is_some() {
            self.select_menu_at(location);
            if let Some(pointer) = self.seat.get_pointer() {
                pointer.motion(
                    self,
                    None,
                    &MotionEvent {
                        location,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
            }
            self.redraw_needed = true;
            return;
        }
        self.update_mouse_gesture(location, time);
        self.update_interactive(location);
        if self.config.focus.follow_mouse
            && self.interactive.is_none()
            && self.mouse_gesture.is_none()
            && self.focus_cycle.is_none()
            && let Some(id) = self
                .pointer_binding_target_at(location)
                .and_then(|target| target.id)
            && self.clients.focused() != Some(id)
            && self
                .clients
                .get(id)
                .is_some_and(|client| client.policy.capabilities.focusable)
        {
            let _ = self.clients.focus(id);
            if self.config.focus.raise_on_focus {
                let _ = self.clients.raise(id);
            }
            self.sync_focus_and_stacking();
        }
        let focus = self.pointer_focus_at(location);
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.motion(
                self,
                focus.clone(),
                &MotionEvent {
                    location,
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
            if let Some((surface, _)) = focus {
                with_pointer_constraint(&surface, &pointer, |constraint| {
                    if let Some(constraint) =
                        constraint.filter(|constraint| !constraint.is_active())
                    {
                        constraint.activate();
                    }
                });
            }
        }
    }

    fn pointer_motion_relative(
        &mut self,
        delta_x: f64,
        delta_y: f64,
        unaccelerated_x: f64,
        unaccelerated_y: f64,
        time: u32,
    ) {
        self.apply_pending_pointer_hint();
        let focus = self.pointer_focus_at(self.pointer_location);
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.relative_motion(
                self,
                focus,
                &RelativeMotionEvent {
                    delta: (delta_x, delta_y).into(),
                    delta_unaccel: (unaccelerated_x, unaccelerated_y).into(),
                    utime: u64::from(time).saturating_mul(1_000),
                },
            );
        }
        let target = self.clamp_point_to_outputs(
            (
                self.pointer_location.x + delta_x,
                self.pointer_location.y + delta_y,
            )
                .into(),
        );
        self.pointer_motion(target.x, target.y, time);
    }

    fn touch_down(
        &mut self,
        location: Point<f64, Logical>,
        slot: smithay::backend::input::TouchSlot,
        time: u32,
    ) {
        let location = self.clamp_point_to_outputs(location);
        let focus = self.pointer_focus_at(location);
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        touch.down(
            self,
            focus,
            &TouchDownEvent {
                slot,
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
    }

    fn touch_motion(
        &mut self,
        location: Point<f64, Logical>,
        slot: smithay::backend::input::TouchSlot,
        time: u32,
    ) {
        let location = self.clamp_point_to_outputs(location);
        let focus = self.pointer_focus_at(location);
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        touch.motion(
            self,
            focus,
            &TouchMotionEvent {
                slot,
                location,
                time,
            },
        );
    }

    fn touch_up(&mut self, slot: smithay::backend::input::TouchSlot, time: u32) {
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        touch.up(
            self,
            &TouchUpEvent {
                slot,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
    }

    fn touch_frame(&mut self) {
        if let Some(touch) = self.seat.get_touch() {
            touch.frame(self);
        }
    }

    fn touch_cancel(&mut self) {
        if let Some(touch) = self.seat.get_touch() {
            touch.cancel(self);
        }
    }

    fn tablet_device_added(&mut self, id: String, descriptor: TabletDescriptor) {
        if self.tablet_devices.contains_key(&id) {
            return;
        }
        if self.tablet_devices.len() >= MAX_TABLET_DEVICES {
            warn!(
                limit = MAX_TABLET_DEVICES,
                device = %descriptor.name,
                "ignoring tablet device after reaching the seat bound"
            );
            return;
        }
        self.seat
            .tablet_seat()
            .add_tablet::<Self>(&self.display_handle, &descriptor);
        self.tablet_devices.insert(id, descriptor);
    }

    fn tablet_device_removed(&mut self, id: &str) {
        let Some(descriptor) = self.tablet_devices.remove(id) else {
            return;
        };
        self.seat.tablet_seat().remove_tablet(&descriptor);
    }

    fn tablet_tool_event(&mut self, input: TabletToolInput) {
        let TabletToolInput {
            device_id,
            tablet_descriptor,
            tool_descriptor,
            location,
            time,
            axes,
            action,
        } = input;
        self.tablet_device_added(device_id.clone(), tablet_descriptor);
        let Some(tablet_descriptor) = self.tablet_devices.get(&device_id).cloned() else {
            return;
        };
        let tablet_seat = self.seat.tablet_seat();
        let Some(tablet) = tablet_seat.get_tablet(&tablet_descriptor) else {
            return;
        };
        let tool = if let Some(tool) = tablet_seat.get_tool(&tool_descriptor) {
            tool
        } else {
            if tablet_seat.count_tools() >= MAX_TABLET_TOOLS {
                warn!(
                    limit = MAX_TABLET_TOOLS,
                    "ignoring tablet tool after reaching the seat bound"
                );
                return;
            }
            let display = self.display_handle.clone();
            tablet_seat.add_tool::<Self>(self, &display, &tool_descriptor)
        };
        if let Some(pressure) = axes.pressure {
            tool.pressure(pressure.clamp(0.0, 1.0));
        }
        if let Some(distance) = axes.distance {
            tool.distance(distance.clamp(0.0, 1.0));
        }
        if let Some(tilt) = axes.tilt {
            tool.tilt(tilt);
        }
        if let Some(rotation) = axes.rotation {
            tool.rotation(rotation);
        }
        if let Some(slider) = axes.slider {
            tool.slider_position(slider.clamp(-1.0, 1.0));
        }
        if let Some(wheel) = axes.wheel {
            tool.wheel(wheel.0, wheel.1);
        }
        let location = self.clamp_point_to_outputs(location);
        let focus = self.pointer_focus_at(location);
        let serial = SERIAL_COUNTER.next_serial();
        match action {
            TabletAction::Axis => tool.motion(location, focus, &tablet, serial, time),
            TabletAction::Proximity(smithay::backend::input::ProximityState::In) => {
                tool.motion(location, focus.clone(), &tablet, serial, time);
                // Smithay queues axes for an ordinary motion after proximity-in.
                tool.motion(location, focus, &tablet, serial, time);
            }
            TabletAction::Proximity(smithay::backend::input::ProximityState::Out) => {
                tool.proximity_out(time);
            }
            TabletAction::Tip(state) => {
                tool.motion(location, focus, &tablet, serial, time);
                match state {
                    smithay::backend::input::TabletToolTipState::Down => {
                        tool.tip_down(serial, time);
                    }
                    smithay::backend::input::TabletToolTipState::Up => tool.tip_up(time),
                }
            }
            TabletAction::Button { button, state } => {
                tool.motion(location, focus, &tablet, serial, time);
                tool.button(button, state, serial, time);
            }
        }
    }

    fn pointer_motion_nested(&mut self, x: f64, y: f64, time: u32) {
        let location: Point<f64, Logical> = (x, y).into();
        let previous = self.nested_pointer_location.replace(location);
        if let Some(previous) = previous {
            let delta = location - previous;
            let hint_applied = self.apply_pending_pointer_hint();
            let focus = self.pointer_focus_at(self.pointer_location);
            if let Some(pointer) = self.seat.get_pointer() {
                pointer.relative_motion(
                    self,
                    focus,
                    &RelativeMotionEvent {
                        delta,
                        delta_unaccel: delta,
                        utime: u64::from(time).saturating_mul(1_000),
                    },
                );
            }
            let target = if hint_applied {
                self.clamp_point_to_outputs(self.pointer_location + delta)
            } else {
                location
            };
            self.pointer_motion(target.x, target.y, time);
        } else {
            self.pointer_motion(x, y, time);
        }
    }

    fn pointer_gesture_swipe_begin(&mut self, fingers: u32, time: u32) {
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.gesture_swipe_begin(
                self,
                &GestureSwipeBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                    fingers,
                },
            );
        }
    }

    fn pointer_gesture_swipe_update(&mut self, delta: Point<f64, Logical>, time: u32) {
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.gesture_swipe_update(self, &GestureSwipeUpdateEvent { time, delta });
        }
    }

    fn pointer_gesture_swipe_end(&mut self, cancelled: bool, time: u32) {
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.gesture_swipe_end(
                self,
                &GestureSwipeEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                    cancelled,
                },
            );
        }
    }

    fn pointer_gesture_pinch_begin(&mut self, fingers: u32, time: u32) {
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.gesture_pinch_begin(
                self,
                &GesturePinchBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                    fingers,
                },
            );
        }
    }

    fn pointer_gesture_pinch_update(
        &mut self,
        delta: Point<f64, Logical>,
        scale: f64,
        rotation: f64,
        time: u32,
    ) {
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.gesture_pinch_update(
                self,
                &GesturePinchUpdateEvent {
                    time,
                    delta,
                    scale,
                    rotation,
                },
            );
        }
    }

    fn pointer_gesture_pinch_end(&mut self, cancelled: bool, time: u32) {
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.gesture_pinch_end(
                self,
                &GesturePinchEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                    cancelled,
                },
            );
        }
    }

    fn pointer_gesture_hold_begin(&mut self, fingers: u32, time: u32) {
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.gesture_hold_begin(
                self,
                &GestureHoldBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                    fingers,
                },
            );
        }
    }

    fn pointer_gesture_hold_end(&mut self, cancelled: bool, time: u32) {
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.gesture_hold_end(
                self,
                &GestureHoldEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                    cancelled,
                },
            );
        }
    }

    fn pointer_button(&mut self, detail: u8, state: ButtonState, time: u32) {
        let Some(button) = (match detail {
            1 => Some(0x110),
            2 => Some(0x112),
            3 => Some(0x111),
            _ => None,
        }) else {
            return;
        };
        self.pointer_button_code(button, state, time);
    }

    fn pointer_axis(&mut self, frame: AxisFrame) {
        let wheel = frame.v120.and_then(|(_, vertical)| match vertical.cmp(&0) {
            std::cmp::Ordering::Less => Some((4, vertical.unsigned_abs())),
            std::cmp::Ordering::Greater => Some((5, vertical.unsigned_abs())),
            std::cmp::Ordering::Equal => None,
        });
        let mut consumed = false;
        if self.menu_session.is_some()
            && let Some((button, amount)) = wheel
        {
            let forward = button == 5;
            for _ in 0..amount.saturating_add(119).saturating_div(120).clamp(1, 16) {
                self.menu_session
                    .as_mut()
                    .expect("checked above")
                    .move_selection(forward);
            }
            self.redraw_needed = true;
            return;
        }
        if let Some((button, amount)) = wheel
            && let Some(target) = self.pointer_binding_target_at(self.pointer_location)
        {
            let invocation = PointerInvocation {
                target,
                start: self.pointer_location,
            };
            let modifiers = self.keyboard_modifiers.clone();
            let steps = amount.saturating_add(119).saturating_div(120).clamp(1, 16);
            for _ in 0..steps {
                for trigger in [
                    MouseTrigger::Press,
                    MouseTrigger::Release,
                    MouseTrigger::Click,
                ] {
                    consumed |= self.dispatch_mouse_binding(
                        invocation, button, &modifiers, trigger, frame.time,
                    );
                }
            }
        }
        if !consumed && let Some(pointer) = self.seat.get_pointer() {
            pointer.axis(self, frame);
        }
        self.redraw_needed |= consumed;
    }

    fn pointer_button_code(&mut self, button: u32, state: ButtonState, time: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let button_number = pointer_button_number(button);
        if self.menu_session.is_some() {
            if state == ButtonState::Pressed {
                if self.menu_entry_at(self.pointer_location).is_some() {
                    self.select_menu_at(self.pointer_location);
                } else {
                    self.menu_session = None;
                }
            } else if button_number == Some(1)
                && self.menu_entry_at(self.pointer_location).is_some()
            {
                self.select_menu_at(self.pointer_location);
                self.activate_selected_menu_entry(time);
            }
            self.redraw_needed = true;
            return;
        }
        if state == ButtonState::Pressed
            && let Some(surface) = pointer.current_focus()
            && let Some(layer) = self.layer_for_surface(&surface)
            && layer.cached_state().keyboard_interactivity != KeyboardInteractivity::None
            && let Some(keyboard) = self.seat.get_keyboard()
        {
            keyboard.set_focus(
                self,
                Some(layer.wl_surface().clone()),
                SERIAL_COUNTER.next_serial(),
            );
        }
        let mut forward = true;
        let mut finish_interactive = false;
        if let Some(button_number) = button_number {
            match state {
                ButtonState::Pressed => {
                    if let Some(target) = self.pointer_binding_target_at(self.pointer_location) {
                        let modifiers = self.keyboard_modifiers.clone();
                        let bound = self.has_mouse_binding(target, button_number, &modifiers);
                        let drag_bound = self
                            .mouse_binding_actions(
                                target,
                                button_number,
                                &modifiers,
                                MouseTrigger::Drag,
                            )
                            .is_some();
                        let invocation = PointerInvocation {
                            target,
                            start: self.pointer_location,
                        };
                        let _ = self.dispatch_mouse_binding(
                            invocation,
                            button_number,
                            &modifiers,
                            MouseTrigger::Press,
                            time,
                        );
                        let content =
                            matches!(target.context, MouseContext::Client | MouseContext::Desktop);
                        forward = content && (!bound || (modifiers.is_empty() && !drag_bound));
                        if bound {
                            self.mouse_gesture = Some(MouseGesture {
                                target,
                                button: button_number,
                                modifiers,
                                start: self.pointer_location,
                                dragged: false,
                                forwarded: forward,
                            });
                        }
                    }
                }
                ButtonState::Released => {
                    if let Some(gesture) = self.mouse_gesture.take() {
                        if gesture.button == button_number {
                            forward = gesture.forwarded;
                            finish_interactive = gesture.dragged;
                            if !gesture.dragged {
                                let invocation = PointerInvocation {
                                    target: gesture.target,
                                    start: gesture.start,
                                };
                                let _ = self.dispatch_mouse_binding(
                                    invocation,
                                    gesture.button,
                                    &gesture.modifiers,
                                    MouseTrigger::Release,
                                    time,
                                );
                                if self.pointer_binding_target_at(self.pointer_location)
                                    == Some(gesture.target)
                                {
                                    self.finish_mouse_click(
                                        MouseClick {
                                            target: gesture.target,
                                            button: gesture.button,
                                            modifiers: gesture.modifiers,
                                            location: self.pointer_location,
                                            time: Instant::now(),
                                        },
                                        invocation,
                                        time,
                                    );
                                } else {
                                    self.last_mouse_click = None;
                                }
                            }
                        } else {
                            self.mouse_gesture = Some(gesture);
                        }
                    }
                }
            }
        }
        let serial = SERIAL_COUNTER.next_serial();
        if forward && state == ButtonState::Pressed {
            self.record_input_serial(serial, pointer.current_focus().as_ref());
        }
        if forward {
            pointer.button(
                self,
                &ButtonEvent {
                    serial,
                    time,
                    button,
                    state,
                },
            );
        }
        if state == ButtonState::Released && (finish_interactive || self.interactive.is_some()) {
            self.finish_interactive();
        }
        self.redraw_needed = true;
    }

    fn menu_entry_at(&self, location: Point<f64, Logical>) -> Option<usize> {
        const BORDER: u32 = 2;
        let (panel, start, rows) = self.menu_layout()?;
        if !geometry_contains_point(panel, location) {
            return None;
        }
        let local_y = location.y
            - f64::from(panel.y)
            - f64::from(BORDER)
            - f64::from(self.config.menu.row_height.max(1));
        if local_y < 0.0 {
            return None;
        }
        let row = usize::try_from(
            (local_y / f64::from(self.config.menu.row_height.max(1))).floor() as u64,
        )
        .ok()?;
        (row < rows).then(|| start.saturating_add(row))
    }

    fn select_menu_at(&mut self, location: Point<f64, Logical>) {
        let Some(index) = self.menu_entry_at(location) else {
            return;
        };
        if self
            .menu_session
            .as_ref()
            .and_then(|session| session.current().menu.entries.get(index))
            .is_some_and(RuntimeMenuEntry::selectable)
        {
            self.menu_session
                .as_mut()
                .expect("checked above")
                .current_mut()
                .selected = index;
        }
    }

    fn start_pointer_interactive(
        &mut self,
        selected: Option<PolicyClientId>,
        kind: InteractiveKind,
        start_pointer: Point<f64, Logical>,
    ) {
        let Some(id) = selected else {
            return;
        };
        let Some(client) = self.clients.get(id).copied() else {
            return;
        };
        let operations = client.operations();
        let allowed = match kind {
            InteractiveKind::Move => operations.movable,
            InteractiveKind::Resize(_) => operations.resizable,
        };
        if !allowed {
            return;
        }
        self.interactive = Some(InteractiveOperation {
            id,
            kind,
            start_pointer,
            start_geometry: client.geometry,
        });
        if matches!(kind, InteractiveKind::Resize(_))
            && let Some(toplevel) = self.toplevel_for_client(id)
        {
            self.apply_state_geometry(
                &toplevel,
                client.geometry,
                Some(xdg_toplevel::State::Resizing),
                true,
            );
        }
    }

    fn snap_move_to_visible_clients(
        &self,
        id: PolicyClientId,
        requested: Geometry,
        resistance: u32,
    ) -> Geometry {
        let Some(client) = self.clients.get(id).copied() else {
            return requested;
        };
        let extents = self.client_decoration_extents(client);
        let outer = extents.outer_geometry(requested);
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
                let client = self.clients.get(candidate).copied()?;
                Some(
                    self.client_decoration_extents(client)
                        .outer_geometry(client.geometry),
                )
            });
        let snapped = outer.snap_movement_to(targets, resistance);
        let content = extents.content_geometry(snapped);
        Geometry::new(content.x, content.y, requested.width, requested.height)
    }

    fn update_interactive(&mut self, location: Point<f64, Logical>) {
        let Some(operation) = self.interactive else {
            return;
        };
        let dx = (location.x - operation.start_pointer.x).round() as i32;
        let dy = (location.y - operation.start_pointer.y).round() as i32;
        let geometry = match operation.kind {
            InteractiveKind::Move => {
                let requested = Geometry::new(
                    operation.start_geometry.x.saturating_add(dx),
                    operation.start_geometry.y.saturating_add(dy),
                    operation.start_geometry.width,
                    operation.start_geometry.height,
                );
                let resistance = self.config.mouse.edge_resistance;
                let requested = if self.config.mouse.snap_to_windows {
                    self.snap_move_to_visible_clients(operation.id, requested, resistance)
                } else {
                    requested
                };
                let Some(client) = self.clients.get(operation.id).copied() else {
                    return;
                };
                let extents = self.client_decoration_extents(client);
                let outer = extents
                    .outer_geometry(requested)
                    .snap_movement(self.work_area(), resistance);
                let content = extents.content_geometry(outer);
                Geometry::new(content.x, content.y, requested.width, requested.height)
            }
            InteractiveKind::Resize(edges) => {
                let edges = policy_resize_edges(edges);
                let Some(client) = self.clients.get(operation.id) else {
                    return;
                };
                let resized = pointer_resize_geometry(
                    operation.start_geometry,
                    edges,
                    dx,
                    dy,
                    self.work_area(),
                    self.config.mouse.edge_resistance,
                );
                let constrained = client
                    .size_hints
                    .constrain(Size::new(resized.width, resized.height));
                let initial_right = operation.start_geometry.x.saturating_add(
                    i32::try_from(operation.start_geometry.width).unwrap_or(i32::MAX),
                );
                let initial_bottom = operation.start_geometry.y.saturating_add(
                    i32::try_from(operation.start_geometry.height).unwrap_or(i32::MAX),
                );
                Geometry::new(
                    if edges.left {
                        initial_right
                            .saturating_sub(i32::try_from(constrained.width).unwrap_or(i32::MAX))
                    } else {
                        resized.x
                    },
                    if edges.top {
                        initial_bottom
                            .saturating_sub(i32::try_from(constrained.height).unwrap_or(i32::MAX))
                    } else {
                        resized.y
                    },
                    constrained.width,
                    constrained.height,
                )
            }
        };
        if self.clients.set_geometry(operation.id, geometry) {
            if let InteractiveKind::Resize(_) = operation.kind
                && let Some(toplevel) = self
                    .windows
                    .iter()
                    .find(|window| window.id == operation.id)
                    .and_then(|window| window.window.toplevel().cloned())
            {
                self.apply_state_geometry(
                    &toplevel,
                    geometry,
                    Some(xdg_toplevel::State::Resizing),
                    true,
                );
            }
            self.sync_focus_and_stacking();
            self.redraw_needed = true;
        }
    }

    fn finish_interactive(&mut self) {
        let Some(operation) = self.interactive.take() else {
            return;
        };
        if let InteractiveKind::Resize(_) = operation.kind
            && let Some(toplevel) = self
                .windows
                .iter()
                .find(|window| window.id == operation.id)
                .and_then(|window| window.window.toplevel().cloned())
            && let Some(geometry) = self.clients.get(operation.id).map(|client| client.geometry)
        {
            self.apply_state_geometry(
                &toplevel,
                geometry,
                Some(xdg_toplevel::State::Resizing),
                false,
            );
        }
    }

    fn cursor_surface_location(&self) -> Option<(WlSurface, Point<i32, Physical>)> {
        let CursorImageStatus::Surface(surface) = &self.cursor_status else {
            return None;
        };
        let hotspot = with_states(surface, |states| {
            states
                .data_map
                .get::<CursorImageSurfaceData>()
                .map(|attributes| attributes.lock().unwrap().hotspot)
                .unwrap_or_default()
        });
        Some((
            surface.clone(),
            (
                (self.pointer_location.x.round() as i32).saturating_sub(hotspot.x),
                (self.pointer_location.y.round() as i32).saturating_sub(hotspot.y),
            )
                .into(),
        ))
    }

    fn dnd_icon_surface_location(&self) -> Option<(WlSurface, Point<i32, Physical>)> {
        self.dnd_icon.as_ref().map(|surface| {
            (
                surface.clone(),
                (
                    self.pointer_location.x.round() as i32,
                    self.pointer_location.y.round() as i32,
                )
                    .into(),
            )
        })
    }

    fn keyboard_key(&mut self, detail: u8, state: KeyState, time: u32) {
        self.keyboard_keycode(Keycode::new(u32::from(detail)), state, time);
    }

    fn resolve_binding_press(&mut self, input: &BindingInput) -> BindingOutcome {
        resolve_configured_binding(&self.config, &mut self.key_chain, input)
    }

    fn handle_menu_key(&mut self, input: &BindingInput, time: u32) {
        let symbol = input
            .symbols
            .first()
            .map(String::as_str)
            .unwrap_or_default();
        match symbol {
            "Escape" => self.menu_session = None,
            "Up" => self
                .menu_session
                .as_mut()
                .expect("checked by caller")
                .move_selection(false),
            "Down" => self
                .menu_session
                .as_mut()
                .expect("checked by caller")
                .move_selection(true),
            "Home" => self
                .menu_session
                .as_mut()
                .expect("checked by caller")
                .select_edge(false),
            "End" => self
                .menu_session
                .as_mut()
                .expect("checked by caller")
                .select_edge(true),
            "Left" => {
                if self
                    .menu_session
                    .as_ref()
                    .is_some_and(|session| session.levels.len() > 1)
                {
                    self.menu_session
                        .as_mut()
                        .expect("checked above")
                        .levels
                        .pop();
                }
            }
            "Right" | "Return" | "KP_Enter" => self.activate_selected_menu_entry(time),
            _ => {
                let mut characters = symbol.chars();
                if let (Some(character), None) = (characters.next(), characters.next()) {
                    let matches = self
                        .menu_session
                        .as_mut()
                        .expect("checked by caller")
                        .select_accelerator(character);
                    if matches == 1 {
                        self.activate_selected_menu_entry(time);
                    }
                }
            }
        }
        self.redraw_needed = true;
    }

    fn activate_selected_menu_entry(&mut self, time: u32) {
        let Some((entry, target)) = self.menu_session.as_ref().map(|session| {
            (
                session.current().menu.entries[session.current().selected].clone(),
                session.target,
            )
        }) else {
            return;
        };
        match entry {
            RuntimeMenuEntry::Submenu { menu, .. } => {
                let menu = match menu {
                    RuntimeSubmenu::Named(id) => self.resolve_menu(&id, target),
                    RuntimeSubmenu::Inline(menu) => Some(*menu),
                };
                let Some(menu) = menu else { return };
                let Some(selected) = menu.entries.iter().position(RuntimeMenuEntry::selectable)
                else {
                    return;
                };
                self.menu_session
                    .as_mut()
                    .expect("session remains active")
                    .levels
                    .push(MenuLevel { menu, selected });
            }
            RuntimeMenuEntry::Item {
                action,
                target: entry_target,
                ..
            } => {
                self.menu_session = None;
                self.execute_runtime_menu_action(action, entry_target.or(target), time);
            }
            RuntimeMenuEntry::Separator { .. } => {}
        }
    }

    fn execute_runtime_menu_action(
        &mut self,
        action: RuntimeMenuAction,
        target: Option<PolicyClientId>,
        time: u32,
    ) {
        match action {
            RuntimeMenuAction::Configured(actions) => {
                let _ = self.run_actions(actions, target, time);
            }
            RuntimeMenuAction::ActivateClient(id) => {
                if let Some(workspace) = self.clients.get(id).and_then(|client| {
                    if let WorkspaceAssignment::Workspace(workspace) = client.workspace {
                        Some(workspace)
                    } else {
                        None
                    }
                }) {
                    self.clients.switch_workspace(workspace);
                }
                let _ = self.clients.set_iconic(id, false);
                let _ = self.clients.set_shaded(id, false);
                let _ = self.clients.focus(id);
                let _ = self.clients.raise(id);
                self.sync_focus_and_stacking();
            }
            RuntimeMenuAction::Dismiss => {}
            RuntimeMenuAction::Exit => {
                self.disposition = RunDisposition::Exit;
                self.exit_requested = true;
            }
            RuntimeMenuAction::SessionLogout => {
                self.disposition = RunDisposition::Exit;
                self.exit_requested = true;
            }
            RuntimeMenuAction::Execute {
                command,
                activation,
            } => {
                self.launch_shell_command(command, activation);
            }
            RuntimeMenuAction::LaunchApplication(application) => {
                self.launch_desktop_application(application);
            }
        }
    }

    fn keyboard_keycode(&mut self, keycode: Keycode, state: KeyState, time: u32) {
        if let Some(keyboard) = self.seat.get_keyboard() {
            let serial = SERIAL_COUNTER.next_serial();
            if state == KeyState::Pressed {
                self.record_input_serial(serial, keyboard.current_focus().as_ref());
            }
            let raw_keycode = keycode.raw();
            let actions = keyboard.input::<Vec<Action>, _>(
                self,
                keycode,
                state,
                serial,
                time,
                |compositor, modifiers, key| {
                    compositor.keyboard_modifiers = active_keyboard_modifiers(modifiers);
                    if state == KeyState::Released {
                        compositor.maybe_finish_focus_cycle();
                        if let Some(index) = compositor
                            .intercepted_keycodes
                            .iter()
                            .position(|candidate| *candidate == raw_keycode)
                        {
                            compositor.intercepted_keycodes.swap_remove(index);
                            return FilterResult::Intercept(Vec::new());
                        }
                        return FilterResult::Forward;
                    }
                    if compositor.seat.keyboard_shortcuts_inhibited() {
                        return FilterResult::Forward;
                    }
                    let input = BindingInput::from_xkb(modifiers, key);
                    if compositor.menu_session.is_some() {
                        compositor.handle_menu_key(&input, time);
                        if !compositor.intercepted_keycodes.contains(&raw_keycode) {
                            compositor.intercepted_keycodes.push(raw_keycode);
                        }
                        return FilterResult::Intercept(Vec::new());
                    }
                    if compositor.focus_cycle.is_some()
                        && input.symbols.iter().any(|symbol| symbol == "Escape")
                    {
                        compositor.cancel_focus_cycle();
                        if !compositor.intercepted_keycodes.contains(&raw_keycode) {
                            compositor.intercepted_keycodes.push(raw_keycode);
                        }
                        return FilterResult::Intercept(Vec::new());
                    }
                    if compositor.keyboard_interactive.is_some() {
                        compositor.handle_keyboard_interactive(&input);
                        if !compositor.intercepted_keycodes.contains(&raw_keycode) {
                            compositor.intercepted_keycodes.push(raw_keycode);
                        }
                        return FilterResult::Intercept(Vec::new());
                    }
                    match compositor.resolve_binding_press(&input) {
                        BindingOutcome::Forward => FilterResult::Forward,
                        BindingOutcome::Intercept(actions) => {
                            if !compositor.intercepted_keycodes.contains(&raw_keycode) {
                                compositor.intercepted_keycodes.push(raw_keycode);
                            }
                            FilterResult::Intercept(actions)
                        }
                    }
                },
            );
            if let Some(actions) = actions
                && state == KeyState::Pressed
            {
                let _ = self.run_actions(actions, self.clients.focused(), time);
            }
        }
    }

    fn run_actions(
        &mut self,
        actions: Vec<Action>,
        target: Option<PolicyClientId>,
        time: u32,
    ) -> ActionFlow {
        self.run_actions_with_invocation(actions, target, time, None)
    }

    fn run_actions_with_pointer(
        &mut self,
        actions: Vec<Action>,
        invocation: PointerInvocation,
        time: u32,
    ) -> ActionFlow {
        self.run_actions_with_invocation(actions, invocation.target.id, time, Some(invocation))
    }

    fn run_actions_with_invocation(
        &mut self,
        actions: Vec<Action>,
        target: Option<PolicyClientId>,
        time: u32,
        pointer: Option<PointerInvocation>,
    ) -> ActionFlow {
        for action in actions {
            if self.run_action(action, target, time, pointer) == ActionFlow::Stop {
                return ActionFlow::Stop;
            }
        }
        ActionFlow::Continue
    }

    fn run_action(
        &mut self,
        action: Action,
        target: Option<PolicyClientId>,
        time: u32,
        pointer: Option<PointerInvocation>,
    ) -> ActionFlow {
        let selected = target.or_else(|| self.clients.focused());
        match action {
            Action::Execute {
                command,
                prompt,
                startup_notify,
            } => {
                let activation = startup_notify.is_some();
                if let Some(prompt) = prompt {
                    self.show_confirmation(
                        prompt,
                        RuntimeMenuAction::Execute {
                            command,
                            activation,
                        },
                    );
                } else {
                    self.launch_shell_command(command, activation);
                }
            }
            Action::LaunchTerminal => {
                self.launch_shell_command(self.config.commands.terminal.clone(), true);
            }
            Action::Screenshot { target } => {
                let command = match target {
                    ScreenshotTarget::Screen => self.config.commands.screenshot.clone(),
                    ScreenshotTarget::Window => self.config.commands.window_screenshot.clone(),
                };
                self.launch_shell_command(command, true);
            }
            Action::ShowMenu { menu } => self.show_menu(&menu, selected, pointer),
            Action::Reconfigure => self.reload_requested = true,
            Action::Restart { command } => {
                self.disposition = RunDisposition::Restart { command };
                self.exit_requested = true;
            }
            Action::SessionLogout { prompt } => {
                if !self.config.commands.session.trim().is_empty() {
                    self.launch_shell_command(self.config.commands.session.clone(), false);
                } else if prompt {
                    self.show_confirmation(
                        "Log out of this session?".to_owned(),
                        RuntimeMenuAction::SessionLogout,
                    );
                } else {
                    self.disposition = RunDisposition::Exit;
                    self.exit_requested = true;
                }
            }
            Action::Debug { message } => info!(debug_message = %message, "debug action"),
            Action::If {
                queries,
                then_actions,
                else_actions,
            } => {
                let actions = if self.action_queries_match(&queries, selected) {
                    then_actions
                } else {
                    else_actions
                };
                return self.run_actions_with_invocation(actions, selected, time, pointer);
            }
            Action::ForEach {
                queries,
                then_actions,
                else_actions,
                none,
            } => {
                let clients = self.clients.management_order().collect::<Vec<_>>();
                let mut matched = false;
                for id in clients {
                    if !self.clients.contains(id) {
                        continue;
                    }
                    let matches = self.action_queries_match(&queries, Some(id));
                    matched |= matches;
                    let actions = if matches {
                        then_actions.clone()
                    } else {
                        else_actions.clone()
                    };
                    if self.run_actions_with_invocation(actions, Some(id), time, pointer)
                        == ActionFlow::Stop
                    {
                        break;
                    }
                }
                if !matched {
                    let _ = self.run_actions_with_invocation(none, selected, time, pointer);
                }
            }
            Action::Stop => return ActionFlow::Stop,
            Action::Close => {
                if let Some(id) = selected
                    && self
                        .clients
                        .get(id)
                        .is_some_and(|client| client.operations().closable)
                    && let Some(toplevel) = self.toplevel_for_client(id)
                {
                    toplevel.send_close();
                }
            }
            Action::Kill => {
                if let Some(id) = selected
                    && let Some(toplevel) = self.toplevel_for_client(id)
                {
                    let _ = toplevel.client().unresponsive();
                }
            }
            Action::Focus { here } => {
                if let Some(id) = selected {
                    if here && !self.clients.is_visible(id) {
                        let workspace =
                            WorkspaceAssignment::Workspace(self.clients.current_workspace());
                        self.clients.assign_workspace_family(id, workspace);
                    }
                    let _ = self.clients.set_iconic(id, false);
                    let _ = self.clients.set_shaded(id, false);
                    let _ = self.clients.focus(id);
                    if self.config.focus.raise_on_focus {
                        let _ = self.clients.raise(id);
                    }
                    self.sync_focus_and_stacking();
                }
            }
            Action::FocusToBottom => {
                if let Some(id) = selected {
                    let _ = self.clients.focus_to_bottom(id);
                }
            }
            Action::Unfocus | Action::FocusFallback => {
                if let Some(id) = selected {
                    self.clients.focus_fallback_from(id);
                    self.sync_focus_and_stacking();
                }
            }
            Action::Raise => {
                if let Some(id) = selected {
                    let _ = self.clients.raise(id);
                    self.sync_focus_and_stacking();
                }
            }
            Action::Lower => {
                if let Some(id) = selected {
                    let _ = self.clients.lower(id);
                    self.sync_focus_and_stacking();
                }
            }
            Action::RaiseLower => {
                if let Some(id) = selected {
                    let top = self.clients.stacking().last() == Some(id);
                    if top {
                        let _ = self.clients.lower(id);
                    } else {
                        let _ = self.clients.raise(id);
                    }
                    self.sync_focus_and_stacking();
                }
            }
            Action::Minimize => {
                if let Some(id) = selected
                    && self
                        .clients
                        .get(id)
                        .is_some_and(|client| client.operations().minimizable)
                {
                    let _ = self.clients.set_iconic(id, true);
                    self.sync_focus_and_stacking();
                }
            }
            Action::Maximize { direction } => self.set_client_maximized(selected, direction, true),
            Action::Unmaximize { direction } => {
                self.set_client_maximized(selected, direction, false);
            }
            Action::ToggleMaximize => {
                if let Some(id) = selected {
                    let enabled = self
                        .clients
                        .get(id)
                        .and_then(|client| client.maximize)
                        .is_none();
                    self.set_client_maximized(Some(id), MaximizeDirection::Both, enabled);
                }
            }
            Action::ToggleMaximizeHorizontal => {
                if let Some(id) = selected {
                    let enabled = !self
                        .clients
                        .get(id)
                        .and_then(|client| client.maximize)
                        .is_some_and(|state| state.horizontal);
                    self.set_client_maximized(Some(id), MaximizeDirection::Horizontal, enabled);
                }
            }
            Action::ToggleMaximizeVertical => {
                if let Some(id) = selected {
                    let enabled = !self
                        .clients
                        .get(id)
                        .and_then(|client| client.maximize)
                        .is_some_and(|state| state.vertical);
                    self.set_client_maximized(Some(id), MaximizeDirection::Vertical, enabled);
                }
            }
            Action::ToggleFullscreen => {
                if let Some(id) = selected {
                    let enabled = self
                        .clients
                        .get(id)
                        .is_some_and(|client| client.fullscreen.is_none());
                    if let Some(geometry) =
                        self.clients
                            .set_fullscreen(id, enabled, self.primary_output().geometry)
                        && let Some(toplevel) = self.toplevel_for_client(id)
                    {
                        self.apply_state_geometry(
                            &toplevel,
                            geometry,
                            Some(xdg_toplevel::State::Fullscreen),
                            enabled,
                        );
                    }
                    self.sync_focus_and_stacking();
                }
            }
            Action::ToggleAlwaysOnTop => self.toggle_client_layer(selected, ClientLayer::Above),
            Action::ToggleAlwaysOnBottom => self.toggle_client_layer(selected, ClientLayer::Below),
            Action::SendToLayer { layer } => {
                if let Some(id) = selected
                    && let Some(operations) = self.clients.get(id).map(|client| client.operations())
                {
                    let layer = match layer {
                        LayerTarget::Below if operations.below => ClientLayer::Below,
                        LayerTarget::Normal => ClientLayer::Normal,
                        LayerTarget::Above if operations.above => ClientLayer::Above,
                        LayerTarget::Below | LayerTarget::Above => return ActionFlow::Continue,
                    };
                    let _ = self.clients.set_layer(id, layer);
                    self.sync_focus_and_stacking();
                }
            }
            Action::Decorate => self.set_client_decoration(selected, DecorationOverride::Default),
            Action::Undecorate => {
                self.set_client_decoration(selected, DecorationOverride::Undecorated);
            }
            Action::ToggleDecorations => {
                if let Some(id) = selected {
                    let _ = self.clients.toggle_decorations(id);
                    self.redraw_needed = true;
                }
            }
            Action::ToggleSticky => {
                if let Some(id) = selected
                    && let Some(client) = self.clients.get(id)
                {
                    let assignment = if client.workspace == WorkspaceAssignment::All {
                        WorkspaceAssignment::Workspace(self.clients.current_workspace())
                    } else {
                        WorkspaceAssignment::All
                    };
                    self.clients.assign_workspace_family(id, assignment);
                    self.sync_focus_and_stacking();
                }
            }
            Action::Shade => self.set_client_shaded(selected, true),
            Action::Unshade => self.set_client_shaded(selected, false),
            Action::ToggleShade => {
                if let Some(id) = selected {
                    let shaded = self.clients.get(id).is_some_and(|client| client.shaded);
                    self.set_client_shaded(Some(id), !shaded);
                }
            }
            Action::ShadeLower => {
                if let Some(id) = selected {
                    if self.clients.get(id).is_some_and(|client| client.shaded) {
                        let _ = self.clients.lower(id);
                        self.sync_focus_and_stacking();
                    } else {
                        self.set_client_shaded(Some(id), true);
                    }
                }
            }
            Action::UnshadeRaise => {
                if let Some(id) = selected {
                    if self.clients.get(id).is_some_and(|client| client.shaded) {
                        self.set_client_shaded(Some(id), false);
                    } else {
                        let _ = self.clients.raise(id);
                        self.sync_focus_and_stacking();
                    }
                }
            }
            Action::ToggleShowDesktop { strict } => {
                let showing = !self.clients.showing_desktop();
                self.show_desktop_strict = showing && strict;
                self.clients.set_showing_desktop(showing);
                self.sync_focus_and_stacking();
            }
            Action::Move => {
                if let Some(pointer) = pointer {
                    self.start_pointer_interactive(selected, InteractiveKind::Move, pointer.start);
                } else {
                    self.start_keyboard_interactive(selected, false);
                }
            }
            Action::Resize { edge } => {
                if let Some(id) = selected
                    && let Some(client) = self.clients.get(id).copied()
                {
                    if let Some(pointer) = pointer {
                        let kind = if client.operations().resizable {
                            InteractiveKind::Resize(
                                edge.map(configured_resize_edge)
                                    .or(pointer.target.resize_edge)
                                    .unwrap_or(xdg_toplevel::ResizeEdge::BottomRight),
                            )
                        } else {
                            InteractiveKind::Move
                        };
                        self.start_pointer_interactive(Some(id), kind, pointer.start);
                    } else {
                        self.start_keyboard_interactive(
                            Some(id),
                            client.policy.capabilities.resizable,
                        );
                    }
                }
            }
            Action::MoveRelative { x, y } => {
                if let Some(id) = selected
                    && let Some(client) = self.clients.get(id).copied()
                    && client.policy.capabilities.movable
                {
                    let bounds = self.work_area();
                    let geometry = client
                        .geometry
                        .translated(x.resolve(bounds.width), y.resolve(bounds.height))
                        .clamp_position(bounds);
                    self.configure_client_geometry(id, geometry);
                }
            }
            Action::ResizeRelative {
                left,
                right,
                top,
                bottom,
            } => {
                if let Some(id) = selected
                    && let Some(client) = self.clients.get(id).copied()
                    && client.policy.capabilities.resizable
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
                    self.configure_client_geometry(id, geometry);
                }
            }
            Action::MoveToEdge { direction } => {
                if let Some(id) = selected
                    && let Some((client, extents, geometry, bounds, obstacles)) =
                        self.edge_action_field(id)
                    && client.policy.capabilities.movable
                {
                    let desired = directional_move_geometry(
                        geometry,
                        bounds,
                        &obstacles,
                        cardinal_direction(direction),
                    );
                    self.configure_client_geometry(id, extents.content_geometry(desired));
                }
            }
            Action::GrowToEdge { direction } => {
                if let Some(id) = selected
                    && let Some((client, _extents, geometry, bounds, obstacles)) =
                        self.edge_action_field(id)
                    && client.policy.capabilities.resizable
                    && !(client.shaded && edge_direction_is_vertical(direction))
                {
                    let cardinal = cardinal_direction(direction);
                    let desired = directional_grow_geometry(
                        geometry,
                        bounds,
                        &obstacles,
                        cardinal,
                        BlockingEdgePolicy::Cross,
                    );
                    let desired = if desired == geometry {
                        directional_shrink_geometry(geometry, bounds, &obstacles, cardinal)
                    } else {
                        desired
                    };
                    self.configure_edge_resize(id, client, geometry, desired);
                }
            }
            Action::GrowToFill => {
                if let Some(id) = selected
                    && let Some((client, extents, geometry, bounds, obstacles)) =
                        self.edge_action_field(id)
                    && client.policy.capabilities.resizable
                    && !client.shaded
                {
                    let desired = grow_to_fill_geometry(geometry, bounds, &obstacles);
                    self.configure_client_geometry(id, extents.content_geometry(desired));
                }
            }
            Action::ShrinkToEdge { direction } => {
                if let Some(id) = selected
                    && let Some((client, _extents, geometry, bounds, obstacles)) =
                        self.edge_action_field(id)
                    && client.policy.capabilities.resizable
                    && !(client.shaded && edge_direction_is_vertical(direction))
                {
                    let desired = directional_shrink_geometry(
                        geometry,
                        bounds,
                        &obstacles,
                        cardinal_direction(direction),
                    );
                    self.configure_edge_resize(id, client, geometry, desired);
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
                if let Some(id) = selected {
                    self.apply_absolute_geometry(
                        id,
                        x,
                        y,
                        width,
                        height,
                        width_basis,
                        height_basis,
                        output,
                    );
                }
            }
            Action::MoveToCenter { output } => {
                if let Some(id) = selected {
                    self.apply_absolute_geometry(
                        id,
                        Some(AxisPosition::Center),
                        Some(AxisPosition::Center),
                        None,
                        None,
                        SizeBasis::Outer,
                        SizeBasis::Outer,
                        output,
                    );
                }
            }
            Action::FocusDirection { direction } => {
                self.focus_direction(selected, direction);
            }
            Action::CycleDirection { direction } => {
                if pointer.is_none() && !self.keyboard_modifiers.is_empty() {
                    self.cycle_focus_directional_session(direction);
                } else {
                    self.focus_direction(selected, direction);
                }
            }
            Action::NextWindow => {
                if pointer.is_none() && !self.keyboard_modifiers.is_empty() {
                    self.cycle_focus_session(true);
                } else {
                    self.cycle_focus_immediately(true);
                }
            }
            Action::PreviousWindow => {
                if pointer.is_none() && !self.keyboard_modifiers.is_empty() {
                    self.cycle_focus_session(false);
                } else {
                    self.cycle_focus_immediately(false);
                }
            }
            Action::PreviousWorkspace => {
                let workspace = self
                    .clients
                    .workspace_in_direction(WorkspaceDirection::Previous);
                self.switch_policy_workspace(workspace);
            }
            Action::NextWorkspace => {
                let workspace = self
                    .clients
                    .workspace_in_direction(WorkspaceDirection::Next);
                self.switch_policy_workspace(workspace);
            }
            Action::LastWorkspace => self.switch_policy_workspace(self.clients.last_workspace()),
            Action::WorkspaceLeft { wrap } => {
                self.switch_grid_workspace(WorkspaceDirection::Left, wrap);
            }
            Action::WorkspaceRight { wrap } => {
                self.switch_grid_workspace(WorkspaceDirection::Right, wrap);
            }
            Action::WorkspaceUp { wrap } => {
                self.switch_grid_workspace(WorkspaceDirection::Up, wrap);
            }
            Action::WorkspaceDown { wrap } => {
                self.switch_grid_workspace(WorkspaceDirection::Down, wrap);
            }
            Action::SwitchWorkspace { workspace } => {
                self.switch_policy_workspace(WorkspaceId::new(workspace.saturating_sub(1)));
            }
            Action::MoveToWorkspace { workspace, follow } => {
                self.move_client_to_workspace(
                    selected,
                    WorkspaceId::new(workspace.saturating_sub(1)),
                    follow,
                );
            }
            Action::MoveToPreviousWorkspace { follow } => {
                let workspace = self
                    .clients
                    .workspace_in_direction(WorkspaceDirection::Previous);
                self.move_client_to_workspace(selected, workspace, follow);
            }
            Action::MoveToNextWorkspace { follow } => {
                let workspace = self
                    .clients
                    .workspace_in_direction(WorkspaceDirection::Next);
                self.move_client_to_workspace(selected, workspace, follow);
            }
            Action::MoveToLastWorkspace { follow } => {
                self.move_client_to_workspace(selected, self.clients.last_workspace(), follow);
            }
            Action::MoveToWorkspaceLeft { follow, wrap } => {
                self.move_client_in_grid(selected, WorkspaceDirection::Left, follow, wrap);
            }
            Action::MoveToWorkspaceRight { follow, wrap } => {
                self.move_client_in_grid(selected, WorkspaceDirection::Right, follow, wrap);
            }
            Action::MoveToWorkspaceUp { follow, wrap } => {
                self.move_client_in_grid(selected, WorkspaceDirection::Up, follow, wrap);
            }
            Action::MoveToWorkspaceDown { follow, wrap } => {
                self.move_client_in_grid(selected, WorkspaceDirection::Down, follow, wrap);
            }
            Action::AddWorkspace { at } => self.change_workspace_set(at, true),
            Action::RemoveWorkspace { at } => self.change_workspace_set(at, false),
            Action::Exit { prompt } => {
                if prompt {
                    self.show_confirmation("Exit nobox?".to_owned(), RuntimeMenuAction::Exit);
                } else {
                    self.disposition = RunDisposition::Exit;
                    self.exit_requested = true;
                }
            }
        }
        ActionFlow::Continue
    }

    fn start_keyboard_interactive(&mut self, id: Option<PolicyClientId>, resize: bool) {
        let Some(id) = id else { return };
        let Some(client) = self.clients.get(id).copied() else {
            return;
        };
        let permitted = if resize {
            client.policy.capabilities.resizable
        } else {
            client.policy.capabilities.movable
        } && client.maximize.is_none()
            && client.fullscreen.is_none()
            && !client.iconic
            && self.clients.is_visible(id);
        if !permitted {
            return;
        }
        self.finish_keyboard_interactive(false);
        self.key_chain = None;
        let kind = if resize {
            KeyboardInteractiveKind::Resize { edge: None }
        } else {
            KeyboardInteractiveKind::Move
        };
        self.keyboard_interactive = Some(KeyboardInteractiveOperation {
            id,
            kind,
            original_geometry: client.geometry,
        });
        if resize && let Some(toplevel) = self.toplevel_for_client(id) {
            self.apply_state_geometry(
                &toplevel,
                client.geometry,
                Some(xdg_toplevel::State::Resizing),
                true,
            );
        }
    }

    fn handle_keyboard_interactive(&mut self, input: &BindingInput) {
        let Some(operation) = self.keyboard_interactive else {
            return;
        };
        if input.has_symbol("Escape") {
            self.finish_keyboard_interactive(true);
            return;
        }
        if input.has_symbol("Return") || input.has_symbol("KP_Enter") {
            self.finish_keyboard_interactive(false);
            return;
        }
        let Some(direction) = binding_cardinal_direction(input) else {
            return;
        };
        let Some(client) = self.clients.get(operation.id).copied() else {
            self.keyboard_interactive = None;
            return;
        };
        match operation.kind {
            KeyboardInteractiveKind::Move => {
                let extents = self.client_decoration_extents(client);
                let outer = extents.outer_geometry(client.geometry);
                let step = if input.modifiers.contains(&KeyboardModifier::Control) {
                    1
                } else {
                    8
                };
                let edge = input.modifiers.contains(&KeyboardModifier::Shift);
                let moved = keyboard_move_geometry(outer, self.work_area(), direction, step, edge);
                let _ = self
                    .clients
                    .set_geometry(operation.id, extents.content_geometry(moved));
                self.sync_focus_and_stacking();
                self.redraw_needed = true;
            }
            KeyboardInteractiveKind::Resize { edge } => {
                if edge.is_none_or(|selected| !cardinal_directions_share_axis(selected, direction))
                {
                    if let Some(operation) = &mut self.keyboard_interactive {
                        operation.kind = KeyboardInteractiveKind::Resize {
                            edge: Some(direction),
                        };
                    }
                    return;
                }
                let selected = edge.expect("a matching resize axis has a selected edge");
                let increment = client.size_hints.increment.map_or(1, |increment| {
                    if cardinal_direction_is_horizontal(direction) {
                        increment.width
                    } else {
                        increment.height
                    }
                });
                let step = if increment > 1 {
                    increment
                } else if input.modifiers.contains(&KeyboardModifier::Control) {
                    1
                } else {
                    8
                };
                let (dx, dy) = cardinal_direction_delta(direction, step);
                let deltas = match selected {
                    CardinalDirection::Left => ResizeDeltas {
                        left: dx.saturating_neg(),
                        ..ResizeDeltas::default()
                    },
                    CardinalDirection::Right => ResizeDeltas {
                        right: dx,
                        ..ResizeDeltas::default()
                    },
                    CardinalDirection::Up => ResizeDeltas {
                        top: dy.saturating_neg(),
                        ..ResizeDeltas::default()
                    },
                    CardinalDirection::Down => ResizeDeltas {
                        bottom: dy,
                        ..ResizeDeltas::default()
                    },
                };
                let geometry = relative_resize_geometry(client.geometry, deltas, client.size_hints)
                    .clamp_position(self.work_area());
                if self.clients.set_geometry(operation.id, geometry) {
                    if let Some(toplevel) = self.toplevel_for_client(operation.id) {
                        self.apply_state_geometry(
                            &toplevel,
                            geometry,
                            Some(xdg_toplevel::State::Resizing),
                            true,
                        );
                    }
                    self.sync_focus_and_stacking();
                }
            }
        }
    }

    fn finish_keyboard_interactive(&mut self, cancel: bool) {
        let Some(operation) = self.keyboard_interactive.take() else {
            return;
        };
        if cancel {
            let _ = self
                .clients
                .set_geometry(operation.id, operation.original_geometry);
        }
        if matches!(operation.kind, KeyboardInteractiveKind::Resize { .. })
            && let Some(geometry) = self.clients.get(operation.id).map(|client| client.geometry)
            && let Some(toplevel) = self.toplevel_for_client(operation.id)
        {
            self.apply_state_geometry(
                &toplevel,
                geometry,
                Some(xdg_toplevel::State::Resizing),
                false,
            );
        }
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
    }

    fn action_query_context(&self, id: PolicyClientId) -> Option<ActionQueryContext<'_>> {
        let client = self.clients.get(id)?;
        let managed = self.windows.iter().find(|managed| managed.id == id)?;
        Some(ActionQueryContext {
            identity: ApplicationIdentity {
                name: &managed.app_id,
                class: &managed.app_id,
                group_name: "",
                group_class: "",
                role: "",
                title: &managed.title,
                kind: application_kind(client.policy.role),
            },
            workspace: match client.workspace {
                WorkspaceAssignment::Workspace(workspace) => Some(workspace.index()),
                WorkspaceAssignment::All => None,
            },
            active_workspace: self.clients.current_workspace().index(),
            last_workspace: self.clients.last_workspace().index(),
            output: 1,
            shaded: client.shaded,
            maximized_horizontal: client.maximize.is_some_and(|state| state.horizontal),
            maximized_vertical: client.maximize.is_some_and(|state| state.vertical),
            minimized: client.iconic,
            fullscreen: client.fullscreen.is_some(),
            focused: self.clients.focused() == Some(id),
            focusable: client.policy.capabilities.focusable,
            urgent: client.presentation.urgent,
            decorated: client.policy.decorations.titlebar,
        })
    }

    fn action_queries_match(
        &self,
        queries: &[ActionQuery],
        target: Option<PolicyClientId>,
    ) -> bool {
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

    fn toplevel_for_client(&self, id: PolicyClientId) -> Option<ToplevelSurface> {
        self.windows
            .iter()
            .find(|managed| managed.id == id)
            .and_then(|managed| managed.window.toplevel().cloned())
    }

    fn configure_client_geometry(&mut self, id: PolicyClientId, geometry: Geometry) {
        if self.clients.set_geometry(id, geometry)
            && let Some(toplevel) = self.toplevel_for_client(id)
        {
            self.apply_state_geometry(&toplevel, geometry, None, false);
        }
        self.sync_focus_and_stacking();
    }

    fn client_decoration_extents(&self, client: PolicyClient) -> DecorationExtents {
        if client.fullscreen.is_some() {
            return DecorationExtents::default();
        }
        client.policy.decorations.extents(
            self.config.theme.border_width,
            self.config.theme.titlebar_height,
        )
    }

    fn edge_action_field(
        &self,
        id: PolicyClientId,
    ) -> Option<(
        PolicyClient,
        DecorationExtents,
        Geometry,
        Geometry,
        Vec<Geometry>,
    )> {
        let client = self.clients.get(id).copied()?;
        let extents = self.client_decoration_extents(client);
        let geometry = extents.outer_geometry(client.geometry);
        let obstacles = self
            .clients
            .stacking()
            .filter(|candidate| {
                *candidate != id
                    && self.clients.is_visible(*candidate)
                    && self
                        .clients
                        .get(*candidate)
                        .is_some_and(|client| !client.iconic)
            })
            .filter_map(|candidate| {
                let client = self.clients.get(candidate).copied()?;
                Some(
                    self.client_decoration_extents(client)
                        .outer_geometry(client.geometry),
                )
            })
            .collect();
        Some((client, extents, geometry, self.work_area(), obstacles))
    }

    fn configure_edge_resize(
        &mut self,
        id: PolicyClientId,
        client: PolicyClient,
        current: Geometry,
        desired: Geometry,
    ) {
        let geometry = relative_resize_geometry(
            client.geometry,
            ResizeDeltas::between(current, desired),
            client.size_hints,
        );
        self.configure_client_geometry(id, geometry);
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_absolute_geometry(
        &mut self,
        id: PolicyClientId,
        x: Option<AxisPosition>,
        y: Option<AxisPosition>,
        width: Option<PositiveRelativeAmount>,
        height: Option<PositiveRelativeAmount>,
        width_basis: SizeBasis,
        height_basis: SizeBasis,
        output: OutputTarget,
    ) {
        let Some(client) = self.clients.get(id).copied() else {
            return;
        };
        let operations = client.operations();
        let wants_resize = width.is_some() || height.is_some();
        if !(operations.movable || operations.resizable && wants_resize) {
            return;
        }
        if matches!(output, OutputTarget::Index(index) if index.get() != 1) {
            warn!(
                ?output,
                "absolute geometry action selected a missing Wayland output"
            );
            return;
        }
        let extents = self.client_decoration_extents(client);
        let current = extents.outer_geometry(client.geometry);
        let bounds = self.work_area();
        let requested = Size::new(
            requested_application_dimension(
                width.filter(|_| operations.resizable),
                width_basis,
                bounds.width,
                client.geometry.width,
                extents.left.saturating_add(extents.right),
            ),
            requested_application_dimension(
                height.filter(|_| operations.resizable),
                height_basis,
                bounds.height,
                client.geometry.height,
                extents.top.saturating_add(extents.bottom),
            ),
        );
        let constrained = client.size_hints.constrain(requested);
        let outer_size =
            extents.outer_geometry(Geometry::new(0, 0, constrained.width, constrained.height));
        let mut outer = move_resize_geometry(
            current,
            bounds,
            bounds,
            Size::new(outer_size.width, outer_size.height),
            x.map_or(AxisPlacement::Keep, |position| {
                axis_placement(position, bounds.width)
            }),
            y.map_or(AxisPlacement::Keep, |position| {
                axis_placement(position, bounds.height)
            }),
        );
        if !operations.movable {
            outer.x = current.x;
            outer.y = current.y;
        }
        self.configure_client_geometry(id, extents.content_geometry(outer));
    }

    fn focus_direction(&mut self, origin: Option<PolicyClientId>, direction: WindowDirection) {
        let candidates = self.clients.focus_cycle_candidates();
        let Some(selected) = self.directional_focus_candidate(origin, &candidates, direction)
        else {
            return;
        };
        let _ = self.clients.set_shaded(selected, false);
        let _ = self.clients.focus(selected);
        let _ = self.clients.raise(selected);
        self.sync_focus_and_stacking();
    }

    fn directional_focus_candidate(
        &self,
        origin: Option<PolicyClientId>,
        candidates: &[PolicyClientId],
        direction: WindowDirection,
    ) -> Option<PolicyClientId> {
        let Some(origin) = origin else {
            return candidates.first().copied();
        };
        let client = self.clients.get(origin).copied()?;
        let origin_geometry = self
            .client_decoration_extents(client)
            .outer_geometry(client.geometry);
        let rectangles = candidates.iter().filter_map(|candidate| {
            let client = self.clients.get(*candidate).copied()?;
            Some((
                *candidate,
                self.client_decoration_extents(client)
                    .outer_geometry(client.geometry),
            ))
        });
        directional_target(
            origin,
            origin_geometry,
            rectangles,
            spatial_direction(direction),
        )
        .or_else(|| candidates.contains(&origin).then_some(origin))
    }

    fn set_client_maximized(
        &mut self,
        id: Option<PolicyClientId>,
        direction: MaximizeDirection,
        enabled: bool,
    ) {
        let Some(id) = id else { return };
        let current = self.clients.get(id).and_then(|client| client.maximize);
        let (horizontal, vertical) = match direction {
            MaximizeDirection::Both => (enabled, enabled),
            MaximizeDirection::Horizontal => (enabled, current.is_some_and(|state| state.vertical)),
            MaximizeDirection::Vertical => (current.is_some_and(|state| state.horizontal), enabled),
        };
        if let Some(geometry) =
            self.clients
                .set_maximized(id, horizontal, vertical, self.work_area())
            && let Some(toplevel) = self.toplevel_for_client(id)
        {
            self.apply_state_geometry(
                &toplevel,
                geometry,
                Some(xdg_toplevel::State::Maximized),
                horizontal && vertical,
            );
        }
        self.sync_focus_and_stacking();
    }

    fn toggle_client_layer(&mut self, id: Option<PolicyClientId>, layer: ClientLayer) {
        if let Some(id) = id
            && let Some(client) = self.clients.get(id)
        {
            let operations = client.operations();
            if (layer == ClientLayer::Above && !operations.above)
                || (layer == ClientLayer::Below && !operations.below)
            {
                return;
            }
            let target = if client.layer == layer {
                ClientLayer::Normal
            } else {
                layer
            };
            let _ = self.clients.set_layer(id, target);
            self.sync_focus_and_stacking();
        }
    }

    fn set_client_decoration(
        &mut self,
        id: Option<PolicyClientId>,
        preference: DecorationOverride,
    ) {
        if let Some(id) = id {
            let _ = self.clients.set_decoration_override(id, preference);
            self.redraw_needed = true;
        }
    }

    fn set_client_shaded(&mut self, id: Option<PolicyClientId>, shaded: bool) {
        if let Some(id) = id {
            let _ = self.clients.set_shaded(id, shaded);
            self.sync_focus_and_stacking();
            self.redraw_needed = true;
        }
    }

    fn cycle_focus_immediately(&mut self, forward: bool) {
        let candidates = self.clients.focus_cycle_candidates();
        if candidates.len() < 2 {
            return;
        }
        let current = self
            .clients
            .focused()
            .and_then(|focused| candidates.iter().position(|id| *id == focused))
            .unwrap_or(0);
        let next = if forward {
            current.saturating_add(1) % candidates.len()
        } else if current == 0 {
            candidates.len() - 1
        } else {
            current - 1
        };
        let id = candidates[next];
        let _ = self.clients.focus(id);
        let _ = self.clients.raise(id);
        self.sync_focus_and_stacking();
    }

    fn cycle_focus_session(&mut self, forward: bool) {
        if !self.prepare_focus_cycle(FocusCycleKind::Linear) {
            return;
        }
        let Some(cycle) = &mut self.focus_cycle else {
            return;
        };
        cycle.index = if forward {
            cycle.index.saturating_add(1) % cycle.candidates.len()
        } else if cycle.index == 0 {
            cycle.candidates.len() - 1
        } else {
            cycle.index - 1
        };
        let selected = cycle.candidates[cycle.index];
        let _ = self.clients.set_shaded(selected, false);
        let _ = self.clients.focus(selected);
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
    }

    fn cycle_focus_directional_session(&mut self, direction: WindowDirection) {
        if !self.prepare_focus_cycle(FocusCycleKind::Spatial) {
            return;
        }
        let Some((origin, candidates)) = self.focus_cycle.as_ref().map(|cycle| {
            (
                cycle.candidates.get(cycle.index).copied(),
                cycle.candidates.clone(),
            )
        }) else {
            return;
        };
        let Some(selected) = self.directional_focus_candidate(origin, &candidates, direction)
        else {
            return;
        };
        if let Some(cycle) = &mut self.focus_cycle
            && let Some(index) = cycle
                .candidates
                .iter()
                .position(|candidate| *candidate == selected)
        {
            cycle.index = index;
        }
        let _ = self.clients.set_shaded(selected, false);
        let _ = self.clients.focus(selected);
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
    }

    fn prepare_focus_cycle(&mut self, kind: FocusCycleKind) -> bool {
        if self
            .focus_cycle
            .as_ref()
            .is_some_and(|cycle| cycle.kind != kind)
        {
            self.finish_focus_cycle();
        }
        if self.focus_cycle.is_some() {
            return true;
        }
        let candidates = self.clients.focus_cycle_candidates();
        if candidates.len() < 2 {
            return false;
        }
        let index = self
            .clients
            .focused()
            .and_then(|focused| candidates.iter().position(|id| *id == focused))
            .unwrap_or(0);
        self.focus_cycle = Some(FocusCycle {
            kind,
            candidates,
            index,
            original: self.clients.focused(),
            modifiers: focus_cycle_modifiers(&self.keyboard_modifiers),
        });
        true
    }

    fn maybe_finish_focus_cycle(&mut self) {
        let released = self.focus_cycle.as_ref().is_some_and(|cycle| {
            cycle
                .modifiers
                .iter()
                .any(|modifier| !self.keyboard_modifiers.contains(modifier))
        });
        if released {
            self.finish_focus_cycle();
        }
    }

    fn finish_focus_cycle(&mut self) {
        let Some(cycle) = self.focus_cycle.take() else {
            return;
        };
        if let Some(selected) = cycle.candidates.get(cycle.index).copied()
            && self.config.focus.raise_on_focus
        {
            let _ = self.clients.raise(selected);
        }
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
    }

    fn cancel_focus_cycle(&mut self) {
        let original = self.focus_cycle.take().and_then(|cycle| cycle.original);
        if let Some(original) = original
            && self.clients.contains(original)
        {
            let _ = self.clients.focus(original);
        }
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
    }

    fn show_menu(
        &mut self,
        id: &str,
        target: Option<PolicyClientId>,
        pointer: Option<PointerInvocation>,
    ) {
        let target = target.or_else(|| self.clients.focused());
        let Some(menu) = self.resolve_menu(id, target) else {
            warn!(menu = id, "ignored unavailable Wayland menu");
            return;
        };
        let bounds = self.work_area();
        let (anchor_x, anchor_y, centered) = pointer.map_or_else(
            || {
                (
                    centered_axis(bounds.x, bounds.width, 1),
                    centered_axis(bounds.y, bounds.height, 1),
                    true,
                )
            },
            |invocation| {
                (
                    invocation.start.x.round() as i32,
                    invocation.start.y.round() as i32,
                    false,
                )
            },
        );
        self.focus_cycle = None;
        self.mouse_gesture = None;
        self.menu_session = MenuSession::new(menu, target, anchor_x, anchor_y, centered);
        self.redraw_needed = true;
    }

    fn show_confirmation(&mut self, title: String, action: RuntimeMenuAction) {
        let menu = RuntimeMenu {
            title,
            entries: vec![
                action_entry("_Cancel", RuntimeMenuAction::Dismiss, None),
                action_entry("_Confirm", action, None),
            ],
        };
        let bounds = self.work_area();
        self.menu_session = MenuSession::new(
            menu,
            None,
            centered_axis(bounds.x, bounds.width, 1),
            centered_axis(bounds.y, bounds.height, 1),
            true,
        );
        self.redraw_needed = true;
    }

    fn menu_definition(&self, id: &str) -> Option<MenuDefinition> {
        self.config
            .menu
            .definitions
            .iter()
            .find(|definition| definition.id == id)
            .cloned()
    }

    fn resolve_menu(&self, id: &str, target: Option<PolicyClientId>) -> Option<RuntimeMenu> {
        let definition = self.menu_definition(id)?;
        let entries = match definition.source {
            MenuSource::Static => definition.entries.iter().map(configured_entry).collect(),
            MenuSource::Command => {
                let command = definition.command.as_deref()?;
                let output = match bounded_shell_output(
                    command,
                    Duration::from_millis(u64::from(self.config.menu.command_timeout_ms)),
                    MAX_COMMAND_MENU_BYTES,
                ) {
                    Ok(output) => output,
                    Err(error) => {
                        warn!(menu = definition.id, %error, "Wayland command menu failed");
                        return None;
                    }
                };
                match self.config.parse_command_menu(&definition.id, &output) {
                    Ok(entries) => entries.iter().map(configured_entry).collect(),
                    Err(error) => {
                        warn!(menu = definition.id, %error, "Wayland command menu returned invalid TOML");
                        return None;
                    }
                }
            }
            MenuSource::Applications => self.resolve_application_menu_entries(&definition),
            MenuSource::Client => self.resolve_client_menu_entries(target?)?,
            MenuSource::ClientWorkspaces => self.resolve_workspace_menu_entries(target?)?,
            MenuSource::Windows => self.resolve_windows_menu_entries(),
        };
        Some(paginate_runtime_menu(
            RuntimeMenu {
                title: definition.title,
                entries,
            },
            self.menu_row_capacity(),
        ))
    }

    fn menu_row_capacity(&self) -> usize {
        const BORDER: u32 = 2;
        const MARGIN: u32 = 20;
        let available = self.work_area().height.saturating_sub(MARGIN).max(1);
        let fitting = available
            .saturating_sub(BORDER.saturating_mul(2))
            .saturating_div(self.config.menu.row_height.max(1))
            .saturating_sub(1)
            .max(1);
        usize::try_from(self.config.menu.max_rows.min(fitting)).unwrap_or(usize::MAX)
    }

    fn resolve_application_menu_entries(
        &self,
        definition: &MenuDefinition,
    ) -> Vec<RuntimeMenuEntry> {
        const MAX_DYNAMIC_ENTRIES: usize = 1_024;
        let mut entries = Vec::with_capacity(self.application_catalog.groups().len());
        let mut remaining = MAX_DYNAMIC_ENTRIES;
        for (index, group) in self.application_catalog.groups().iter().enumerate() {
            if remaining <= 1 {
                break;
            }
            let application_count = group.applications.len().min(remaining - 1);
            if application_count == 0 {
                continue;
            }
            let title = group.category.title();
            let category = RuntimeMenu {
                title: title.to_owned(),
                entries: group.applications[..application_count]
                    .iter()
                    .cloned()
                    .map(|application| {
                        let label = application.name.clone();
                        action_entry(
                            &label,
                            RuntimeMenuAction::LaunchApplication(application),
                            None,
                        )
                    })
                    .collect(),
            };
            entries.push(submenu_entry(
                title,
                RuntimeSubmenu::Inline(Box::new(category)),
            ));
            remaining = remaining.saturating_sub(application_count.saturating_add(1));
            let _ = index;
        }
        if entries.is_empty() {
            entries.push(action_entry(
                "No applications found",
                RuntimeMenuAction::Dismiss,
                None,
            ));
        }
        debug!(
            menu = definition.id,
            applications = self.application_catalog.application_count(),
            skipped = self.application_catalog.skipped_files(),
            "discovered Wayland application menu"
        );
        entries
    }

    fn resolve_client_menu_entries(&self, target: PolicyClientId) -> Option<Vec<RuntimeMenuEntry>> {
        let client = self.clients.get(target).copied()?;
        let operations = client.operations();
        let mut entries = Vec::with_capacity(14);
        if operations.workspace_movable {
            entries.push(submenu_entry(
                "_Send to workspace",
                RuntimeSubmenu::Named("client-workspaces".to_owned()),
            ));
        }
        for (enabled, label, action) in [
            (operations.minimizable, "Mi_nimize", Action::Minimize),
            (
                operations.maximizable,
                if client.maximize.is_some() {
                    "Unma_ximize"
                } else {
                    "Ma_ximize"
                },
                Action::ToggleMaximize,
            ),
            (
                operations.shadeable,
                if client.shaded { "Uns_hade" } else { "S_hade" },
                Action::ToggleShade,
            ),
            (
                operations.fullscreenable,
                if client.fullscreen.is_some() {
                    "Leave _fullscreen"
                } else {
                    "_Fullscreen"
                },
                Action::ToggleFullscreen,
            ),
            (true, "_Raise", Action::Raise),
            (true, "_Lower", Action::Lower),
            (operations.closable, "_Close", Action::Close),
        ] {
            if enabled {
                entries.push(action_entry(
                    label,
                    RuntimeMenuAction::Configured(vec![action]),
                    Some(target),
                ));
            }
        }
        Some(entries)
    }

    fn resolve_workspace_menu_entries(
        &self,
        target: PolicyClientId,
    ) -> Option<Vec<RuntimeMenuEntry>> {
        self.clients.get(target)?;
        let mut entries = self
            .config
            .workspaces
            .names
            .iter()
            .enumerate()
            .filter_map(|(index, name)| {
                let workspace = u32::try_from(index).ok()?.checked_add(1)?;
                Some(action_entry(
                    &format!("{workspace}: {name}"),
                    RuntimeMenuAction::Configured(vec![Action::MoveToWorkspace {
                        workspace,
                        follow: true,
                    }]),
                    Some(target),
                ))
            })
            .collect::<Vec<_>>();
        entries.push(action_entry(
            "_All workspaces",
            RuntimeMenuAction::Configured(vec![Action::ToggleSticky]),
            Some(target),
        ));
        Some(entries)
    }

    fn resolve_windows_menu_entries(&self) -> Vec<RuntimeMenuEntry> {
        self.clients
            .management_order()
            .filter(|id| {
                self.clients
                    .get(*id)
                    .is_some_and(|client| !client.presentation.skip_taskbar)
            })
            .take(512)
            .map(|id| {
                let title = self
                    .windows
                    .iter()
                    .find(|managed| managed.id == id)
                    .map(|managed| managed.title.as_str())
                    .filter(|title| !title.is_empty())
                    .unwrap_or("(untitled)");
                action_entry(title, RuntimeMenuAction::ActivateClient(id), Some(id))
            })
            .collect()
    }

    fn remove_focus_cycle_candidate(&mut self, removed: PolicyClientId) {
        let Some(cycle) = &mut self.focus_cycle else {
            return;
        };
        let selected = cycle.candidates.get(cycle.index).copied();
        cycle.candidates.retain(|candidate| *candidate != removed);
        if cycle.original == Some(removed) {
            cycle.original = None;
        }
        if cycle.candidates.is_empty() {
            self.focus_cycle = None;
            return;
        }
        cycle.index = selected
            .and_then(|selected| {
                cycle
                    .candidates
                    .iter()
                    .position(|candidate| *candidate == selected)
            })
            .unwrap_or_else(|| cycle.index.min(cycle.candidates.len() - 1));
        if cycle.candidates.len() < 2 {
            self.finish_focus_cycle();
        }
    }

    fn switch_policy_workspace(&mut self, workspace: WorkspaceId) {
        if self.clients.switch_workspace(workspace) {
            self.sync_workspace_protocol();
            self.sync_focus_and_stacking();
        }
    }

    fn switch_grid_workspace(&mut self, direction: WorkspaceDirection, wrap: Option<bool>) {
        let workspace = self
            .clients
            .workspace_in_grid_direction(direction, wrap.unwrap_or(self.config.workspaces.wrap));
        self.switch_policy_workspace(workspace);
    }

    fn move_client_to_workspace(
        &mut self,
        id: Option<PolicyClientId>,
        workspace: WorkspaceId,
        follow: bool,
    ) {
        let Some(id) = id else { return };
        if workspace.index() >= self.clients.workspace_count() {
            return;
        }
        self.clients
            .assign_workspace_family(id, WorkspaceAssignment::Workspace(workspace));
        if follow {
            let _ = self.clients.switch_workspace(workspace);
            let _ = self.clients.focus(id);
        }
        self.sync_workspace_protocol();
        self.sync_focus_and_stacking();
    }

    fn move_client_in_grid(
        &mut self,
        id: Option<PolicyClientId>,
        direction: WorkspaceDirection,
        follow: bool,
        wrap: Option<bool>,
    ) {
        let workspace = self
            .clients
            .workspace_in_grid_direction(direction, wrap.unwrap_or(self.config.workspaces.wrap));
        self.move_client_to_workspace(id, workspace, follow);
    }

    fn change_workspace_set(&mut self, at: WorkspacePlacement, insert: bool) {
        let index = match at {
            WorkspacePlacement::Current => self.clients.current_workspace().index(),
            WorkspacePlacement::Last if insert => self.clients.workspace_count(),
            WorkspacePlacement::Last => self.clients.workspace_count().saturating_sub(1),
        };
        let changed = if insert {
            self.clients.insert_workspace(WorkspaceId::new(index))
        } else {
            self.clients.remove_workspace(WorkspaceId::new(index))
        };
        if !changed {
            return;
        }
        if insert {
            let name = (self.config.workspaces.names.len().saturating_add(1)).to_string();
            self.config
                .workspaces
                .names
                .insert(usize::try_from(index).unwrap_or(usize::MAX), name);
        } else if let Ok(index) = usize::try_from(index) {
            self.config.workspaces.names.remove(index);
        }
        self.clients
            .set_workspace_layout(configured_workspace_layout(&self.config));
        self.sync_workspace_protocol();
        self.sync_focus_and_stacking();
    }

    fn record_input_serial(&mut self, serial: Serial, surface: Option<&WlSurface>) {
        const MAX_SERIALS: usize = 64;
        const MAX_AGE: Duration = Duration::from_secs(5);
        let Some(client_id) = surface
            .and_then(WlSurface::client)
            .map(|client| client.id())
        else {
            return;
        };
        let now = Instant::now();
        self.recent_input_serials
            .retain(|entry| now.duration_since(entry.created) <= MAX_AGE);
        self.recent_input_serials.push_back(RecentInputSerial {
            serial,
            client_id,
            created: now,
        });
        while self.recent_input_serials.len() > MAX_SERIALS {
            self.recent_input_serials.pop_front();
        }
    }

    fn valid_activation_token(&self, data: &XdgActivationTokenData) -> bool {
        const MAX_AGE: Duration = Duration::from_secs(5);
        if data.timestamp.elapsed() > MAX_AGE {
            return false;
        }
        if !self.config.focus.prevent_focus_stealing {
            return true;
        }
        let (Some(client_id), Some((serial, seat))) = (&data.client_id, &data.serial) else {
            return false;
        };
        self.seat.owns(seat)
            && self.recent_input_serials.iter().any(|entry| {
                entry.serial == *serial
                    && entry.client_id == *client_id
                    && entry.created.elapsed() <= MAX_AGE
            })
    }

    fn prune_activation_tokens(&mut self) {
        const MAX_AGE: Duration = Duration::from_secs(5);
        self.xdg_activation_state
            .retain_tokens(|_, data| data.timestamp.elapsed() <= MAX_AGE);
        let known = self
            .xdg_activation_state
            .tokens()
            .map(|(token, _)| token.clone())
            .collect::<HashSet<_>>();
        self.trusted_activation_tokens
            .retain(|token| known.contains(token));
    }

    fn launch_activation_token(&mut self) -> Option<String> {
        const MAX_TOKENS: usize = 256;
        self.prune_activation_tokens();
        if self.xdg_activation_state.tokens().count() >= MAX_TOKENS {
            warn!("could not allocate a bounded Wayland launch activation token");
            return None;
        }
        let token = {
            let (token, _) = self.xdg_activation_state.create_external_token(None);
            token.clone()
        };
        let value = token.as_str().to_owned();
        self.trusted_activation_tokens.insert(token);
        Some(value)
    }

    fn launch_shell_command(&mut self, command: String, activation: bool) {
        let token = activation.then(|| self.launch_activation_token()).flatten();
        spawn_shell_command(&command, &self.wayland_display, token.as_deref());
    }

    fn launch_desktop_application(&mut self, application: DesktopApplication) {
        let token = application
            .startup_notify
            .then(|| self.launch_activation_token())
            .flatten();
        spawn_desktop_application(application, &self.wayland_display, token.as_deref());
    }

    fn decoration_elements(&self) -> Vec<SolidColorRenderElement> {
        let mut elements = Vec::new();
        for managed in &self.windows {
            let Some(client) = self.clients.get(managed.id) else {
                continue;
            };
            if !self.clients.is_visible(managed.id)
                || client.iconic
                || !client.policy.decorations.is_present()
                || client.fullscreen.is_some()
            {
                continue;
            }
            let width = i32::try_from(client.geometry.width).unwrap_or(i32::MAX);
            let height = i32::try_from(client.geometry.height).unwrap_or(i32::MAX);
            let focused = self.clients.focused() == Some(managed.id);
            let border_color = if client.presentation.urgent {
                color(self.config.theme.urgent_border)
            } else if focused {
                color(self.config.theme.active_border)
            } else {
                color(self.config.theme.inactive_border)
            };
            let title_color = if client.presentation.urgent {
                color(self.config.theme.urgent_titlebar)
            } else if focused {
                color(self.config.theme.active_titlebar)
            } else {
                color(self.config.theme.inactive_titlebar)
            };
            let extents = self.client_decoration_extents(*client);
            let border_width = i32::try_from(extents.left).unwrap_or(i32::MAX);
            let titlebar_height =
                i32::try_from(extents.top.saturating_sub(extents.left)).unwrap_or(i32::MAX);
            let border = SolidColorBuffer::new(
                (
                    width.saturating_add(border_width.saturating_mul(2)),
                    height
                        .saturating_add(titlebar_height)
                        .saturating_add(border_width.saturating_mul(2)),
                ),
                border_color,
            );
            elements.push(SolidColorRenderElement::from_buffer(
                &border,
                (
                    client.geometry.x.saturating_sub(border_width),
                    client
                        .geometry
                        .y
                        .saturating_sub(titlebar_height)
                        .saturating_sub(border_width),
                ),
                1.0,
                1.0,
                Kind::Unspecified,
            ));
            if titlebar_height > 0 {
                let buttons = frame_button_geometries(*client, &self.config);
                let title = SolidColorBuffer::new((width, titlebar_height), title_color);
                elements.push(SolidColorRenderElement::from_buffer(
                    &title,
                    (
                        client.geometry.x,
                        client.geometry.y.saturating_sub(titlebar_height),
                    ),
                    1.0,
                    1.0,
                    Kind::Unspecified,
                ));
                for (button, geometry) in &buttons {
                    let button_color = match button {
                        FrameButton::Minimize => color(self.config.theme.minimize_button),
                        FrameButton::Maximize => color(self.config.theme.maximize_button),
                        FrameButton::Close => color(self.config.theme.close_button),
                    };
                    if let Some(element) =
                        solid_geometry_element(*geometry, button_color, Kind::Unspecified)
                    {
                        elements.push(element);
                    }
                    for glyph in frame_button_glyph(*button, *geometry) {
                        if let Some(element) = solid_geometry_element(
                            glyph,
                            color(self.config.theme.button_glyph),
                            Kind::Unspecified,
                        ) {
                            elements.push(element);
                        }
                    }
                }
                let titlebar = Geometry::new(
                    client.geometry.x,
                    client.geometry.y.saturating_sub(titlebar_height),
                    client.geometry.width,
                    u32::try_from(titlebar_height).unwrap_or(0),
                );
                let padding = self.config.theme.title_padding.min(titlebar.width);
                let button_space =
                    buttons
                        .iter()
                        .map(|(_, geometry)| geometry.x)
                        .min()
                        .map_or(0, |button_x| {
                            let titlebar_right = i64::from(titlebar.x) + i64::from(titlebar.width);
                            u32::try_from(titlebar_right.saturating_sub(i64::from(button_x)))
                                .unwrap_or(u32::MAX)
                        });
                if let (Some(renderer), Some(clip)) = (
                    &self.text_renderer,
                    horizontal_inset(titlebar, padding, padding.saturating_add(button_space)),
                ) {
                    let pixels = u16::try_from(
                        u32::try_from(titlebar_height)
                            .unwrap_or(1)
                            .saturating_mul(3)
                            .saturating_div(5)
                            .clamp(8, 24),
                    )
                    .unwrap_or(12);
                    let origin = text_origin(
                        clip,
                        renderer.measure(&managed.title, pixels),
                        self.config.theme.title_alignment,
                    );
                    let text_color = color(self.config.theme.title_text);
                    for run in renderer.runs(&managed.title, origin, clip, pixels) {
                        if let Some(element) = solid_geometry_element(
                            run.geometry,
                            covered_color(text_color, run.coverage),
                            Kind::Unspecified,
                        ) {
                            elements.push(element);
                        }
                    }
                }
            }
        }
        elements
    }

    fn overlay_elements(&self) -> Vec<SolidColorRenderElement> {
        let mut elements = self.switcher_elements();
        elements.extend(self.menu_elements());
        if let CursorImageStatus::Named(icon) = &self.cursor_status {
            let location = Point::<i32, Logical>::from((
                self.pointer_location.x.round() as i32,
                self.pointer_location.y.round() as i32,
            ));
            for geometry in named_cursor_geometries(*icon, location) {
                if let Some(element) =
                    solid_geometry_element(geometry, [0.92, 0.94, 0.98, 1.0], Kind::Cursor)
                {
                    elements.push(element);
                }
            }
        }
        elements
    }

    fn menu_layout(&self) -> Option<(Geometry, usize, usize)> {
        const BORDER: u32 = 2;
        const MARGIN: u32 = 20;
        let session = self.menu_session.as_ref()?;
        let level = session.current();
        let bounds = self.work_area();
        let row_height = self.config.menu.row_height.max(1);
        let available_height = bounds.height.saturating_sub(MARGIN).max(1);
        let fitting = available_height
            .saturating_sub(BORDER.saturating_mul(2))
            .saturating_div(row_height)
            .saturating_sub(1)
            .max(1);
        let rows = level
            .menu
            .entries
            .len()
            .min(usize::try_from(self.config.menu.max_rows.min(fitting)).unwrap_or(usize::MAX));
        let width = self
            .config
            .menu
            .width
            .min(bounds.width.saturating_sub(MARGIN).max(1));
        let height = row_height
            .saturating_mul(u32::try_from(rows.saturating_add(1)).unwrap_or(u32::MAX))
            .saturating_add(BORDER.saturating_mul(2))
            .min(available_height);
        let x = if session.centered {
            centered_axis(bounds.x, bounds.width, width)
        } else {
            place_popup_axis(session.anchor_x, bounds.x, bounds.width, width)
        };
        let y = if session.centered {
            centered_axis(bounds.y, bounds.height, height)
        } else {
            place_popup_axis(session.anchor_y, bounds.y, bounds.height, height)
        };
        Some((
            Geometry::new(x, y, width, height),
            focus_cycle_visible_start(level.menu.entries.len(), level.selected, rows),
            rows,
        ))
    }

    fn menu_elements(&self) -> Vec<SolidColorRenderElement> {
        const BORDER: u32 = 2;
        let Some(session) = &self.menu_session else {
            return Vec::new();
        };
        let Some((panel, start, rows)) = self.menu_layout() else {
            return Vec::new();
        };
        let level = session.current();
        let row_height = self.config.menu.row_height.max(1);
        let mut elements = Vec::new();
        if let Some(element) = solid_geometry_element(
            panel,
            color(self.config.theme.active_border),
            Kind::Unspecified,
        ) {
            elements.push(element);
        }
        let content_width = panel.width.saturating_sub(BORDER.saturating_mul(2));
        let title = Geometry::new(
            panel
                .x
                .saturating_add(i32::try_from(BORDER).unwrap_or(i32::MAX)),
            panel
                .y
                .saturating_add(i32::try_from(BORDER).unwrap_or(i32::MAX)),
            content_width,
            row_height,
        );
        if let Some(element) = solid_geometry_element(
            title,
            color(self.config.theme.active_titlebar),
            Kind::Unspecified,
        ) {
            elements.push(element);
        }
        self.append_overlay_text(&mut elements, &level.menu.title, title);
        for (row, entry) in level.menu.entries.iter().skip(start).take(rows).enumerate() {
            let row = u32::try_from(row).unwrap_or(u32::MAX);
            let geometry = Geometry::new(
                title.x,
                title.y.saturating_add(
                    i32::try_from(row.saturating_add(1).saturating_mul(row_height))
                        .unwrap_or(i32::MAX),
                ),
                content_width,
                row_height,
            );
            let index = start.saturating_add(usize::try_from(row).unwrap_or(usize::MAX));
            let background = if index == level.selected {
                self.config.theme.active_titlebar
            } else {
                self.config.theme.inactive_titlebar
            };
            if let Some(element) =
                solid_geometry_element(geometry, color(background), Kind::Unspecified)
            {
                elements.push(element);
            }
            if let Some(label) = entry.label() {
                let label = if matches!(entry, RuntimeMenuEntry::Submenu { .. }) {
                    format!("{label}  ›")
                } else {
                    label.to_owned()
                };
                self.append_overlay_text(&mut elements, &label, geometry);
            } else if matches!(entry, RuntimeMenuEntry::Separator { .. }) {
                let line = Geometry::new(
                    geometry.x.saturating_add(6),
                    geometry
                        .y
                        .saturating_add(i32::try_from(geometry.height / 2).unwrap_or(i32::MAX)),
                    geometry.width.saturating_sub(12),
                    1,
                );
                if let Some(element) = solid_geometry_element(
                    line,
                    color(self.config.theme.inactive_border),
                    Kind::Unspecified,
                ) {
                    elements.push(element);
                }
            }
        }
        elements
    }

    fn append_overlay_text(
        &self,
        elements: &mut Vec<SolidColorRenderElement>,
        text: &str,
        row: Geometry,
    ) {
        let padding = self.config.theme.title_padding.min(row.width);
        let (Some(renderer), Some(clip)) =
            (&self.text_renderer, horizontal_inset(row, padding, padding))
        else {
            return;
        };
        let pixels = u16::try_from(row.height.saturating_mul(3).saturating_div(5).clamp(8, 24))
            .unwrap_or(12);
        let text_color = color(self.config.theme.title_text);
        for run in renderer.runs(text, clip.x, clip, pixels) {
            if let Some(element) = solid_geometry_element(
                run.geometry,
                covered_color(text_color, run.coverage),
                Kind::Unspecified,
            ) {
                elements.push(element);
            }
        }
    }

    fn switcher_elements(&self) -> Vec<SolidColorRenderElement> {
        const BORDER: u32 = 2;
        const OUTER_MARGIN: u32 = 40;

        let Some(cycle) = self
            .focus_cycle
            .as_ref()
            .filter(|_| self.config.switcher.enabled)
        else {
            return Vec::new();
        };
        if cycle.candidates.is_empty() || cycle.index >= cycle.candidates.len() {
            return Vec::new();
        }
        let bounds = self.work_area();
        let row_height = self.config.switcher.row_height.max(1);
        let available_height = bounds.height.saturating_sub(OUTER_MARGIN).max(1);
        let fitting_rows = available_height
            .saturating_sub(BORDER.saturating_mul(2))
            .saturating_div(row_height)
            .max(1);
        let rows = cycle.candidates.len().min(
            usize::try_from(self.config.switcher.max_rows.min(fitting_rows)).unwrap_or(usize::MAX),
        );
        let width = self
            .config
            .switcher
            .width
            .min(bounds.width.saturating_sub(OUTER_MARGIN).max(1));
        let content_height = row_height.saturating_mul(u32::try_from(rows).unwrap_or(u32::MAX));
        let height = content_height
            .saturating_add(BORDER.saturating_mul(2))
            .min(available_height)
            .max(1);
        let panel = Geometry::new(
            centered_axis(bounds.x, bounds.width, width),
            centered_axis(bounds.y, bounds.height, height),
            width,
            height,
        );
        let mut elements = Vec::new();
        if let Some(element) = solid_geometry_element(
            panel,
            color(self.config.theme.active_border),
            Kind::Unspecified,
        ) {
            elements.push(element);
        }
        if let Some(selected) = cycle.candidates.get(cycle.index).copied()
            && let Some(client) = self.clients.get(selected).copied()
        {
            let outer = self
                .client_decoration_extents(client)
                .outer_geometry(client.geometry);
            let thickness = self.config.theme.border_width.max(2);
            for geometry in outline_geometries(outer, thickness) {
                if let Some(element) = solid_geometry_element(
                    geometry,
                    color(self.config.theme.active_border),
                    Kind::Unspecified,
                ) {
                    elements.push(element);
                }
            }
        }
        let start = focus_cycle_visible_start(cycle.candidates.len(), cycle.index, rows);
        for (row, candidate) in cycle.candidates.iter().skip(start).take(rows).enumerate() {
            let row = u32::try_from(row).unwrap_or(u32::MAX);
            let geometry = Geometry::new(
                panel
                    .x
                    .saturating_add(i32::try_from(BORDER).unwrap_or(i32::MAX)),
                panel.y.saturating_add(
                    i32::try_from(BORDER.saturating_add(row.saturating_mul(row_height)))
                        .unwrap_or(i32::MAX),
                ),
                panel.width.saturating_sub(BORDER.saturating_mul(2)),
                row_height,
            );
            let selected =
                start.saturating_add(usize::try_from(row).unwrap_or(usize::MAX)) == cycle.index;
            let background = if selected {
                self.config.theme.active_titlebar
            } else {
                self.config.theme.inactive_titlebar
            };
            if let Some(element) =
                solid_geometry_element(geometry, color(background), Kind::Unspecified)
            {
                elements.push(element);
            }
            let Some(managed) = self.windows.iter().find(|managed| managed.id == *candidate) else {
                continue;
            };
            let padding = self.config.theme.title_padding.min(geometry.width);
            let (Some(renderer), Some(clip)) = (
                &self.text_renderer,
                horizontal_inset(geometry, padding, padding),
            ) else {
                continue;
            };
            let pixels = u16::try_from(row_height.saturating_mul(3).saturating_div(5).clamp(8, 24))
                .unwrap_or(12);
            let origin = text_origin(
                clip,
                renderer.measure(&managed.title, pixels),
                TitleAlignment::Left,
            );
            let text_color = color(self.config.theme.title_text);
            for run in renderer.runs(&managed.title, origin, clip, pixels) {
                if let Some(element) = solid_geometry_element(
                    run.geometry,
                    covered_color(text_color, run.coverage),
                    Kind::Unspecified,
                ) {
                    elements.push(element);
                }
            }
        }
        elements
    }

    fn apply_state_geometry(
        &mut self,
        surface: &ToplevelSurface,
        geometry: Geometry,
        state: Option<xdg_toplevel::State>,
        enabled: bool,
    ) {
        surface.with_pending_state(|pending| {
            pending.size = Some(
                (
                    i32::try_from(geometry.width).unwrap_or(i32::MAX),
                    i32::try_from(geometry.height).unwrap_or(i32::MAX),
                )
                    .into(),
            );
            if let Some(state) = state {
                if enabled {
                    pending.states.set(state);
                } else {
                    pending.states.unset(state);
                }
            }
        });
        surface.send_pending_configure();
        self.redraw_needed = true;
    }

    fn check_client_liveness(&mut self) {
        const PING_INTERVAL: Duration = Duration::from_secs(1);
        const PING_TIMEOUT: Duration = Duration::from_secs(2);
        let now = Instant::now();
        for managed in &mut self.windows {
            let Some(toplevel) = managed.window.toplevel() else {
                continue;
            };
            let client = toplevel.client();
            match managed.pending_ping {
                Some((_serial, sent)) if now.duration_since(sent) >= PING_TIMEOUT => {
                    if client.unresponsive().is_ok() {
                        warn!(
                            client = managed.id.raw(),
                            "disconnecting unresponsive Wayland client"
                        );
                    }
                    managed.pending_ping = None;
                }
                Some(_) => {}
                None if now.duration_since(managed.last_ping) >= PING_INTERVAL => {
                    let serial = SERIAL_COUNTER.next_serial();
                    if client.send_ping(serial).is_ok() {
                        managed.pending_ping = Some((serial, now));
                        managed.last_ping = now;
                    }
                }
                None => {}
            }
        }
    }
}

struct ManagedWindow {
    id: PolicyClientId,
    window: Window,
    title: String,
    app_id: String,
    foreign_toplevel: Option<ForeignToplevelHandle>,
    last_ping: Instant,
    pending_ping: Option<(Serial, Instant)>,
}

struct FocusCycle {
    kind: FocusCycleKind,
    candidates: Vec<PolicyClientId>,
    index: usize,
    original: Option<PolicyClientId>,
    modifiers: Vec<KeyboardModifier>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FocusCycleKind {
    Linear,
    Spatial,
}

struct RecentInputSerial {
    serial: Serial,
    client_id: ClientId,
    created: Instant,
}

struct WorkspaceManagerInstance {
    manager: ext_workspace_manager_v1::ExtWorkspaceManagerV1,
    group: ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1,
    workspaces: Vec<WorkspaceResource>,
}

struct WorkspaceResource {
    handle: ext_workspace_handle_v1::ExtWorkspaceHandleV1,
    index: u32,
}

#[derive(Clone, Copy)]
struct WorkspaceResourceData {
    index: u32,
}

#[derive(Clone, Copy)]
struct InteractiveOperation {
    id: PolicyClientId,
    kind: InteractiveKind,
    start_pointer: Point<f64, Logical>,
    start_geometry: Geometry,
}

#[derive(Clone, Copy)]
enum InteractiveKind {
    Move,
    Resize(xdg_toplevel::ResizeEdge),
}

#[derive(Clone, Copy)]
struct KeyboardInteractiveOperation {
    id: PolicyClientId,
    kind: KeyboardInteractiveKind,
    original_geometry: Geometry,
}

#[derive(Clone, Copy)]
enum KeyboardInteractiveKind {
    Move,
    Resize { edge: Option<CardinalDirection> },
}

impl BufferHandler for Compositor {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl DmabufHandler for Compositor {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        if self.pending_dmabuf_imports.len() >= MAX_PENDING_DMABUF_IMPORTS {
            warn!(
                limit = MAX_PENDING_DMABUF_IMPORTS,
                "rejecting DMA-BUF import after reaching the pending-import bound"
            );
            notifier.failed();
            return;
        }
        self.pending_dmabuf_imports
            .push_back(PendingDmabufImport { dmabuf, notifier });
    }
}

impl DrmSyncobjHandler for Compositor {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.drm_syncobj_state.as_mut()
    }
}

impl CompositorHandler for Compositor {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<WaylandClientState>()
            .expect("all Wayland clients are inserted with WaylandClientState")
            .compositor_state
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        let Some(client) = surface.client() else {
            return;
        };
        let client_state = client
            .get_data::<WaylandClientState>()
            .expect("all Wayland clients are inserted with WaylandClientState");
        if !reserve_bounded(&client_state.surface_count, MAX_CLIENT_SURFACES) {
            surface.post_error(
                0_u32,
                format!("client exceeded the {MAX_CLIENT_SURFACES}-surface limit"),
            );
            return;
        }
        let count = Arc::clone(&client_state.surface_count);
        let inserted = with_states(surface, |states| {
            states
                .data_map
                .insert_if_missing_threadsafe::<CountedSurface, _>(|| CountedSurface {
                    count: Arc::clone(&count),
                    active: AtomicBool::new(true),
                })
        });
        if !inserted {
            release_reservation(&count);
        }
        add_pre_commit_hook::<Self, _>(surface, |state, _, surface| {
            state.prepare_syncobj_commit(surface);
        });
        self.set_surface_preferred_scale(surface);
    }

    fn commit(&mut self, surface: &WlSurface) {
        self.cache_pointer_constraint_hint(surface);
        on_commit_buffer_handler::<Self>(surface);
        self.queue_surface_import(surface);
        self.popup_manager.commit(surface);
        self.map_toplevel_if_ready(surface);
        self.commit_layer_surface(surface);
        self.popup_manager.cleanup();
        for output in &self.outputs {
            layer_map_for_output(&output.output).cleanup();
        }
        self.redraw_needed = true;
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        if self.dnd_icon.as_ref() == Some(surface) {
            self.dnd_icon = None;
            self.redraw_needed = true;
        }
        with_states(surface, |states| {
            if let Some(counted) = states.data_map.get::<CountedSurface>()
                && counted.active.swap(false, Ordering::AcqRel)
            {
                release_reservation(&counted.count);
            }
        });
    }
}

impl FractionalScaleHandler for Compositor {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        self.set_surface_preferred_scale(&surface);
    }
}

impl ShmHandler for Compositor {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl OutputHandler for Compositor {}

impl XdgShellHandler for Compositor {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.bounds = Some(
                (
                    i32::try_from(self.primary_output().geometry.width).unwrap_or(i32::MAX),
                    i32::try_from(self.primary_output().geometry.height).unwrap_or(i32::MAX),
                )
                    .into(),
            );
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        surface.send_configure();
        let id = PolicyClientId::new(self.next_client_id);
        self.next_client_id = self.next_client_id.saturating_add(1);
        self.windows.push(ManagedWindow {
            id,
            window: Window::new_wayland_window(surface),
            title: String::new(),
            app_id: String::new(),
            foreign_toplevel: None,
            last_ping: Instant::now(),
            pending_ping: None,
        });
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let geometry = positioner.get_unconstrained_geometry(Rectangle::new(
            (0, 0).into(),
            (
                i32::try_from(self.primary_output().geometry.width).unwrap_or(i32::MAX),
                i32::try_from(self.primary_output().geometry.height).unwrap_or(i32::MAX),
            )
                .into(),
        ));
        surface.with_pending_state(|state| state.geometry = geometry);
        if let Err(error) = surface.send_configure() {
            warn!(?error, "could not configure xdg popup");
        }
        if let Err(error) = self.popup_manager.track_popup(surface.into()) {
            warn!(?error, "could not track xdg popup");
        }
        self.redraw_needed = true;
    }

    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, serial: Serial) {
        if !self.valid_interactive_request(&surface, serial) {
            return;
        }
        let Some(id) = self.window_for_toplevel(&surface).map(|window| window.id) else {
            return;
        };
        let Some(client) = self.clients.get(id).copied() else {
            return;
        };
        if !client.operations().movable {
            return;
        }
        self.interactive = Some(InteractiveOperation {
            id,
            kind: InteractiveKind::Move,
            start_pointer: self.pointer_location,
            start_geometry: client.geometry,
        });
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        if !self.valid_interactive_request(&surface, serial) {
            return;
        }
        let Some(id) = self.window_for_toplevel(&surface).map(|window| window.id) else {
            return;
        };
        let Some(client) = self.clients.get(id).copied() else {
            return;
        };
        if !client.operations().resizable {
            return;
        }
        self.interactive = Some(InteractiveOperation {
            id,
            kind: InteractiveKind::Resize(edges),
            start_pointer: self.pointer_location,
            start_geometry: client.geometry,
        });
    }

    fn grab(&mut self, surface: PopupSurface, _seat: wl_seat::WlSeat, serial: Serial) {
        let pointer_valid = self
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.has_grab(serial));
        let keyboard_valid = self
            .seat
            .get_keyboard()
            .is_some_and(|keyboard| keyboard.has_grab(serial));
        if !pointer_valid && !keyboard_valid {
            return;
        }
        let popup = surface.into();
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        match self
            .popup_manager
            .grab_popup(root, popup, &self.seat, serial)
        {
            Ok(grab) => {
                if let Some(pointer) = self.seat.get_pointer() {
                    pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Clear);
                }
                if let Some(keyboard) = self.seat.get_keyboard() {
                    keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
                }
            }
            Err(error) => warn!(?error, "rejected xdg popup grab"),
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        let geometry = positioner.get_unconstrained_geometry(Rectangle::new(
            (0, 0).into(),
            (
                i32::try_from(self.primary_output().geometry.width).unwrap_or(i32::MAX),
                i32::try_from(self.primary_output().geometry.height).unwrap_or(i32::MAX),
            )
                .into(),
        ));
        surface.with_pending_state(|state| state.geometry = geometry);
        if let Err(error) = surface.send_configure() {
            warn!(?error, "could not reconfigure xdg popup");
        }
        surface.send_repositioned(token);
        self.redraw_needed = true;
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let Some(id) = self.window_for_toplevel(&surface).map(|window| window.id) else {
            return;
        };
        if let Some(geometry) = self.clients.set_maximized(id, true, true, self.work_area()) {
            self.apply_state_geometry(
                &surface,
                geometry,
                Some(xdg_toplevel::State::Maximized),
                true,
            );
            self.sync_focus_and_stacking();
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        let Some(id) = self.window_for_toplevel(&surface).map(|window| window.id) else {
            return;
        };
        if let Some(geometry) = self
            .clients
            .set_maximized(id, false, false, self.work_area())
        {
            self.apply_state_geometry(
                &surface,
                geometry,
                Some(xdg_toplevel::State::Maximized),
                false,
            );
            self.sync_focus_and_stacking();
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        let Some(id) = self.window_for_toplevel(&surface).map(|window| window.id) else {
            return;
        };
        if let Some(geometry) =
            self.clients
                .set_fullscreen(id, true, self.primary_output().geometry)
        {
            self.apply_state_geometry(
                &surface,
                geometry,
                Some(xdg_toplevel::State::Fullscreen),
                true,
            );
            self.sync_focus_and_stacking();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let Some(id) = self.window_for_toplevel(&surface).map(|window| window.id) else {
            return;
        };
        if let Some(geometry) =
            self.clients
                .set_fullscreen(id, false, self.primary_output().geometry)
        {
            self.apply_state_geometry(
                &surface,
                geometry,
                Some(xdg_toplevel::State::Fullscreen),
                false,
            );
            self.sync_focus_and_stacking();
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        let Some(id) = self.window_for_toplevel(&surface).map(|window| window.id) else {
            return;
        };
        let _ = self.clients.set_iconic(id, true);
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(index) = self.windows.iter().position(|managed| {
            managed.window.wl_surface().as_deref() == Some(surface.wl_surface())
        }) {
            let mut managed = self.windows.remove(index);
            self.space.unmap_elem(&managed.window);
            if let Some(handle) = managed.foreign_toplevel.take() {
                self.foreign_toplevel_list_state.remove_toplevel(&handle);
            }
            let _ = self.clients.unmanage(managed.id);
            self.session_stacking.remove(&managed.id);
            self.remove_focus_cycle_candidate(managed.id);
            self.sync_focus_and_stacking();
            self.redraw_needed = true;
        }
    }

    fn client_pong(&mut self, client: ShellClient) {
        for managed in &mut self.windows {
            if managed
                .window
                .toplevel()
                .is_some_and(|surface| surface.client() == client)
            {
                managed.pending_ping = None;
                managed.last_ping = Instant::now();
            }
        }
    }
}

impl XdgDecorationHandler for Compositor {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_pending_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        self.new_decoration(toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.new_decoration(toplevel);
    }
}

impl ForeignToplevelListHandler for Compositor {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_list_state
    }
}

impl XdgActivationHandler for Compositor {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, _data: XdgActivationTokenData) -> bool {
        const MAX_TOKENS: usize = 256;
        self.prune_activation_tokens();
        self.xdg_activation_state.tokens().count() < MAX_TOKENS
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        let trusted = self.trusted_activation_tokens.remove(&token);
        self.xdg_activation_state.remove_token(&token);
        let Some(id) = self.surface_window(&surface).map(|managed| managed.id) else {
            return;
        };
        if !trusted && !self.valid_activation_token(&token_data) {
            if self.clients.focused() != Some(id)
                && let Some(mut presentation) =
                    self.clients.get(id).map(|client| client.presentation)
            {
                presentation.urgent = true;
                let _ = self.clients.set_presentation(id, presentation);
                self.redraw_needed = true;
            }
            return;
        }
        if let Some(workspace) = self
            .clients
            .get(id)
            .and_then(|client| match client.workspace {
                WorkspaceAssignment::Workspace(workspace) => Some(workspace),
                WorkspaceAssignment::All => None,
            })
        {
            self.clients.switch_workspace(workspace);
        }
        let _ = self.clients.set_iconic(id, false);
        let _ = self.clients.focus(id);
        let _ = self.clients.raise(id);
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
    }
}

impl WlrLayerShellHandler for Compositor {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        output: Option<WlOutput>,
        _layer: WlrLayer,
        namespace: String,
    ) {
        let output = output
            .as_ref()
            .and_then(Output::from_resource)
            .filter(|requested| {
                self.outputs
                    .iter()
                    .any(|output| output.output == *requested)
            })
            .unwrap_or_else(|| self.primary_output().output.clone());
        let layer = DesktopLayerSurface::new(surface, bounded_protocol_text(Some(&namespace), 256));
        self.layer_surfaces.push(ManagedLayerSurface {
            surface: layer,
            output,
        });
        self.redraw_needed = true;
    }

    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        if let Err(error) = self.popup_manager.track_popup(popup.into()) {
            warn!(?error, "could not track layer-shell popup");
        }
        self.redraw_needed = true;
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let Some(index) = self
            .layer_surfaces
            .iter()
            .position(|layer| layer.surface.layer_surface() == &surface)
        else {
            return;
        };
        let layer = self.layer_surfaces.remove(index);
        layer_map_for_output(&layer.output).unmap_layer(&layer.surface);
        self.redraw_needed = true;
    }
}

impl GlobalDispatch<ext_workspace_manager_v1::ExtWorkspaceManagerV1, (), Compositor>
    for Compositor
{
    fn bind(
        state: &mut Compositor,
        display: &DisplayHandle,
        client: &Client,
        resource: New<ext_workspace_manager_v1::ExtWorkspaceManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Compositor>,
    ) {
        let manager = data_init.init(resource, ());
        let Ok(group) = client.create_resource::<
            ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1,
            _,
            Compositor,
        >(display, manager.version(), ())
        else {
            return;
        };
        manager.workspace_group(&group);
        group.capabilities(ext_workspace_group_handle_v1::GroupCapabilities::empty());
        state.workspace_instances.push(WorkspaceManagerInstance {
            manager,
            group,
            workspaces: Vec::new(),
        });
        state.sync_workspace_protocol();
    }
}

impl Dispatch<ext_workspace_manager_v1::ExtWorkspaceManagerV1, (), Compositor> for Compositor {
    fn request(
        state: &mut Compositor,
        client: &Client,
        manager: &ext_workspace_manager_v1::ExtWorkspaceManagerV1,
        request: ext_workspace_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Compositor>,
    ) {
        match request {
            ext_workspace_manager_v1::Request::Commit => {
                let client_id = client.id();
                let activation = state
                    .pending_workspace_activations
                    .iter()
                    .rev()
                    .find(|(pending_client, _)| *pending_client == client_id)
                    .map(|(_, workspace)| *workspace);
                state
                    .pending_workspace_activations
                    .retain(|(pending_client, _)| *pending_client != client_id);
                if let Some(workspace) = activation
                    && workspace < u32::try_from(state.config.workspaces.names.len()).unwrap_or(1)
                {
                    state.clients.switch_workspace(WorkspaceId::new(workspace));
                    state.sync_focus_and_stacking();
                    state.sync_workspace_protocol();
                    state.redraw_needed = true;
                }
            }
            ext_workspace_manager_v1::Request::Stop => {
                manager.finished();
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Compositor,
        _client_id: ClientId,
        manager: &ext_workspace_manager_v1::ExtWorkspaceManagerV1,
        _data: &(),
    ) {
        state
            .workspace_instances
            .retain(|instance| instance.manager != *manager);
    }
}

impl Dispatch<ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1, (), Compositor>
    for Compositor
{
    fn request(
        _state: &mut Compositor,
        _client: &Client,
        _group: &ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1,
        _request: ext_workspace_group_handle_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Compositor>,
    ) {
    }
}

impl Dispatch<ext_workspace_handle_v1::ExtWorkspaceHandleV1, WorkspaceResourceData, Compositor>
    for Compositor
{
    fn request(
        state: &mut Compositor,
        client: &Client,
        _workspace: &ext_workspace_handle_v1::ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        data: &WorkspaceResourceData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Compositor>,
    ) {
        if matches!(request, ext_workspace_handle_v1::Request::Activate) {
            state
                .pending_workspace_activations
                .push((client.id(), data.index));
        }
    }
}

impl Dispatch<WlSeat, SeatUserData<Self>> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &WlSeat,
        request: wl_seat::Request,
        data: &SeatUserData<Self>,
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, wl_seat::Request::GetTouch { .. }) {
            let client_state = client
                .get_data::<WaylandClientState>()
                .expect("all Wayland clients are inserted with WaylandClientState");
            if !reserve_bounded(&client_state.touch_device_count, MAX_CLIENT_TOUCH_DEVICES) {
                resource.post_error(
                    0_u32,
                    format!("client exceeded the {MAX_CLIENT_TOUCH_DEVICES}-touch-device limit"),
                );
            }
        }
        <SeatState<Self> as Dispatch<WlSeat, SeatUserData<Self>, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }

    fn destroyed(state: &mut Self, client: ClientId, resource: &WlSeat, data: &SeatUserData<Self>) {
        <SeatState<Self> as Dispatch<WlSeat, SeatUserData<Self>, Self>>::destroyed(
            state, client, resource, data,
        );
    }
}

impl SeatHandler for Compositor {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        if let Some(inhibitor) = self.active_shortcuts_inhibitor.take() {
            inhibitor.inactivate();
        }
        if let Some(inhibitor) =
            focused.and_then(|surface| seat.keyboard_shortcuts_inhibitor_for_surface(surface))
        {
            inhibitor.activate();
            self.active_shortcuts_inhibitor = Some(inhibitor);
        }
        let focused_client = focused.and_then(|surface| surface.client());
        set_data_device_focus(&self.display_handle, seat, focused_client.clone());
        set_primary_focus(&self.display_handle, seat, focused_client);
        if let Some(id) =
            focused.and_then(|surface| self.surface_window(surface).map(|window| window.id))
        {
            let _ = self.clients.focus(id);
            if self.focus_cycle.is_none() && self.config.focus.raise_on_focus {
                let _ = self.clients.raise(id);
            }
        }
        self.redraw_needed = true;
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
        self.redraw_needed = true;
    }
}

impl TabletSeatHandler for Compositor {
    fn tablet_tool_image(
        &mut self,
        _tool: &smithay::backend::input::TabletToolDescriptor,
        image: CursorImageStatus,
    ) {
        self.cursor_status = image;
        self.redraw_needed = true;
    }
}

impl SelectionHandler for Compositor {
    type SelectionUserData = ();

    fn new_selection(
        &mut self,
        target: smithay::wayland::selection::SelectionTarget,
        source: Option<smithay::wayland::selection::SelectionSource>,
        seat: Seat<Self>,
    ) {
        let owner = source.is_some().then(|| {
            seat.get_keyboard()
                .and_then(|keyboard| keyboard.current_focus())
                .and_then(|surface| surface.client())
                .map(|client| client.id())
        });
        let owner = owner.flatten();
        match target {
            smithay::wayland::selection::SelectionTarget::Clipboard => {
                self.clipboard_owner = owner;
            }
            smithay::wayland::selection::SelectionTarget::Primary => {
                self.primary_selection_owner = owner;
            }
        }
    }
}

impl PointerConstraintsHandler for Compositor {
    fn new_constraint(
        &mut self,
        surface: &WlSurface,
        pointer: &smithay::input::pointer::PointerHandle<Self>,
    ) {
        if pointer.current_focus().as_ref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &smithay::input::pointer::PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        if let Some((_focused, origin)) = self.pointer_focus_at(self.pointer_location) {
            self.pending_pointer_hint = Some(origin + location);
        }
    }
}

impl DataDeviceHandler for Compositor {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for Compositor {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl ClientDndGrabHandler for Compositor {
    fn started(
        &mut self,
        _source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        self.dnd_icon = icon;
        self.redraw_needed = true;
    }

    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.dnd_icon = None;
        self.redraw_needed = true;
    }
}

impl ServerDndGrabHandler for Compositor {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

impl KeyboardShortcutsInhibitHandler for Compositor {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        let focused = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        if focused.as_ref() == Some(inhibitor.wl_surface()) {
            inhibitor.activate();
            self.active_shortcuts_inhibitor = Some(inhibitor);
        }
    }

    fn inhibitor_destroyed(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        if self.active_shortcuts_inhibitor.as_ref() == Some(&inhibitor) {
            self.active_shortcuts_inhibitor = None;
        }
    }
}

impl Dispatch<ZwpKeyboardShortcutsInhibitManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpKeyboardShortcutsInhibitManagerV1,
        request: shortcuts_inhibit_manager::Request,
        data: &(),
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(
            request,
            shortcuts_inhibit_manager::Request::InhibitShortcuts { .. }
        ) {
            let client_state = client
                .get_data::<WaylandClientState>()
                .expect("all Wayland clients are inserted with WaylandClientState");
            if !reserve_bounded(
                &client_state.shortcut_inhibitor_count,
                MAX_CLIENT_SHORTCUT_INHIBITORS,
            ) {
                resource.post_error(
                    0_u32,
                    format!(
                        "client exceeded the {MAX_CLIENT_SHORTCUT_INHIBITORS}-shortcut-inhibitor limit"
                    ),
                );
            }
        }
        <KeyboardShortcutsInhibitState as Dispatch<
            ZwpKeyboardShortcutsInhibitManagerV1,
            (),
            Self,
        >>::request(state, client, resource, request, data, display, data_init);
    }
}

impl Dispatch<wp_presentation::WpPresentation, u32> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &wp_presentation::WpPresentation,
        request: wp_presentation::Request,
        data: &u32,
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, wp_presentation::Request::Feedback { .. }) {
            let client_state = client
                .get_data::<WaylandClientState>()
                .expect("all Wayland clients are inserted with WaylandClientState");
            if !reserve_bounded(
                &client_state.presentation_feedback_count,
                MAX_CLIENT_PRESENTATION_FEEDBACKS,
            ) {
                resource.post_error(
                    0_u32,
                    format!(
                        "client exceeded the {MAX_CLIENT_PRESENTATION_FEEDBACKS}-presentation-feedback limit"
                    ),
                );
            }
        }
        <PresentationState as Dispatch<wp_presentation::WpPresentation, u32, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl Dispatch<ZwpRelativePointerManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpRelativePointerManagerV1,
        request: relative_pointer_manager::Request,
        data: &(),
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(
            request,
            relative_pointer_manager::Request::GetRelativePointer { .. }
        ) {
            let client_state = client
                .get_data::<WaylandClientState>()
                .expect("all Wayland clients are inserted with WaylandClientState");
            if !reserve_bounded(
                &client_state.pointer_extension_count,
                MAX_CLIENT_POINTER_EXTENSION_OBJECTS,
            ) {
                resource.post_error(
                    0_u32,
                    format!(
                        "client exceeded the {MAX_CLIENT_POINTER_EXTENSION_OBJECTS}-pointer-extension-object limit"
                    ),
                );
            }
        }
        <RelativePointerManagerState as Dispatch<ZwpRelativePointerManagerV1, (), Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl Dispatch<ZwpPointerConstraintsV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpPointerConstraintsV1,
        request: pointer_constraints_manager::Request,
        data: &(),
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(
            request,
            pointer_constraints_manager::Request::LockPointer { .. }
                | pointer_constraints_manager::Request::ConfinePointer { .. }
        ) {
            let client_state = client
                .get_data::<WaylandClientState>()
                .expect("all Wayland clients are inserted with WaylandClientState");
            if !reserve_bounded(
                &client_state.pointer_extension_count,
                MAX_CLIENT_POINTER_EXTENSION_OBJECTS,
            ) {
                resource.post_error(
                    0_u32,
                    format!(
                        "client exceeded the {MAX_CLIENT_POINTER_EXTENSION_OBJECTS}-pointer-extension-object limit"
                    ),
                );
            }
        }
        <PointerConstraintsState as Dispatch<ZwpPointerConstraintsV1, (), Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl Dispatch<ZwpPointerGesturesV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpPointerGesturesV1,
        request: pointer_gestures_manager::Request,
        data: &(),
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(
            request,
            pointer_gestures_manager::Request::GetSwipeGesture { .. }
                | pointer_gestures_manager::Request::GetPinchGesture { .. }
                | pointer_gestures_manager::Request::GetHoldGesture { .. }
        ) {
            let client_state = client
                .get_data::<WaylandClientState>()
                .expect("all Wayland clients are inserted with WaylandClientState");
            if !reserve_bounded(
                &client_state.pointer_gesture_count,
                MAX_CLIENT_POINTER_GESTURES,
            ) {
                resource.post_error(
                    0_u32,
                    format!(
                        "client exceeded the {MAX_CLIENT_POINTER_GESTURES}-pointer-gesture limit"
                    ),
                );
            }
        }
        <PointerGesturesState as Dispatch<ZwpPointerGesturesV1, (), Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl Dispatch<WpCursorShapeManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &WpCursorShapeManagerV1,
        request: cursor_shape_manager::Request,
        data: &(),
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(
            request,
            cursor_shape_manager::Request::GetPointer { .. }
                | cursor_shape_manager::Request::GetTabletToolV2 { .. }
        ) {
            let client_state = client
                .get_data::<WaylandClientState>()
                .expect("all Wayland clients are inserted with WaylandClientState");
            if !reserve_bounded(&client_state.cursor_shape_count, MAX_CLIENT_CURSOR_SHAPES) {
                resource.post_error(
                    0_u32,
                    format!(
                        "client exceeded the {MAX_CLIENT_CURSOR_SHAPES}-cursor-shape-device limit"
                    ),
                );
            }
        }
        <CursorShapeManagerState as Dispatch<WpCursorShapeManagerV1, (), Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl Dispatch<ZwpTabletManagerV2, ()> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpTabletManagerV2,
        request: tablet_manager::Request,
        data: &(),
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, tablet_manager::Request::GetTabletSeat { .. }) {
            let client_state = client
                .get_data::<WaylandClientState>()
                .expect("all Wayland clients are inserted with WaylandClientState");
            if !reserve_bounded(&client_state.tablet_seat_count, MAX_CLIENT_TABLET_SEATS) {
                resource.post_error(
                    0_u32,
                    format!("client exceeded the {MAX_CLIENT_TABLET_SEATS}-tablet-seat limit"),
                );
            }
        }
        <TabletManagerState as Dispatch<ZwpTabletManagerV2, (), Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl Dispatch<WlDataDeviceManager, ()> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &WlDataDeviceManager,
        request: wl_data_device_manager::Request,
        data: &(),
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let creates_source = matches!(
            request,
            wl_data_device_manager::Request::CreateDataSource { .. }
        );
        let creates_device = matches!(
            request,
            wl_data_device_manager::Request::GetDataDevice { .. }
        );
        let client_state = client
            .get_data::<WaylandClientState>()
            .expect("all Wayland clients are inserted with WaylandClientState");
        if creates_source
            && !reserve_bounded(
                &client_state.selection_source_count,
                MAX_CLIENT_SELECTION_SOURCES,
            )
        {
            resource.post_error(
                0_u32,
                format!(
                    "client exceeded the {MAX_CLIENT_SELECTION_SOURCES}-selection-source limit"
                ),
            );
        }
        if creates_device
            && !reserve_bounded(
                &client_state.selection_device_count,
                MAX_CLIENT_SELECTION_DEVICES,
            )
        {
            resource.post_error(
                0_u32,
                format!(
                    "client exceeded the {MAX_CLIENT_SELECTION_DEVICES}-selection-device limit"
                ),
            );
        }
        <DataDeviceState as Dispatch<WlDataDeviceManager, (), Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl Dispatch<WlDataDevice, DataDeviceUserData> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &WlDataDevice,
        request: wl_data_device::Request,
        data: &DataDeviceUserData,
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        <DataDeviceState as Dispatch<WlDataDevice, DataDeviceUserData, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl Dispatch<WlDataSource, DataSourceUserData> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &WlDataSource,
        request: wl_data_source::Request,
        data: &DataSourceUserData,
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_data_source::Request::Offer { mime_type } = &request {
            let count = state
                .selection_mime_counts
                .entry(resource.id())
                .or_default();
            if mime_type.len() > MAX_MIME_TYPE_BYTES {
                resource.post_error(
                    0_u32,
                    format!("selection MIME type exceeds the {MAX_MIME_TYPE_BYTES}-byte limit"),
                );
                return;
            }
            if *count >= MAX_SOURCE_MIME_TYPES {
                resource.post_error(
                    0_u32,
                    format!(
                        "selection source exceeded the {MAX_SOURCE_MIME_TYPES}-MIME-type limit"
                    ),
                );
                return;
            }
            *count += 1;
        }
        <DataDeviceState as Dispatch<WlDataSource, DataSourceUserData, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &WlDataSource,
        data: &DataSourceUserData,
    ) {
        state.selection_mime_counts.remove(&resource.id());
        <DataDeviceState as Dispatch<WlDataSource, DataSourceUserData, Self>>::destroyed(
            state, client, resource, data,
        );
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpPrimarySelectionDeviceManagerV1,
        request: primary_device_manager::Request,
        data: &(),
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let creates_source = matches!(
            request,
            primary_device_manager::Request::CreateSource { .. }
        );
        let creates_device = matches!(request, primary_device_manager::Request::GetDevice { .. });
        let client_state = client
            .get_data::<WaylandClientState>()
            .expect("all Wayland clients are inserted with WaylandClientState");
        if creates_source
            && !reserve_bounded(
                &client_state.selection_source_count,
                MAX_CLIENT_SELECTION_SOURCES,
            )
        {
            resource.post_error(
                0_u32,
                format!(
                    "client exceeded the {MAX_CLIENT_SELECTION_SOURCES}-selection-source limit"
                ),
            );
        }
        if creates_device
            && !reserve_bounded(
                &client_state.selection_device_count,
                MAX_CLIENT_SELECTION_DEVICES,
            )
        {
            resource.post_error(
                0_u32,
                format!(
                    "client exceeded the {MAX_CLIENT_SELECTION_DEVICES}-selection-device limit"
                ),
            );
        }
        <PrimarySelectionState as Dispatch<ZwpPrimarySelectionDeviceManagerV1, (), Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceV1, PrimaryDeviceUserData> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpPrimarySelectionDeviceV1,
        request: primary_device::Request,
        data: &PrimaryDeviceUserData,
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        <PrimarySelectionState as Dispatch<
            ZwpPrimarySelectionDeviceV1,
            PrimaryDeviceUserData,
            Self,
        >>::request(state, client, resource, request, data, display, data_init);
    }
}

impl Dispatch<ZwpPrimarySelectionSourceV1, PrimarySourceUserData> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpPrimarySelectionSourceV1,
        request: primary_source::Request,
        data: &PrimarySourceUserData,
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let primary_source::Request::Offer { mime_type } = &request {
            let count = state
                .selection_mime_counts
                .entry(resource.id())
                .or_default();
            if mime_type.len() > MAX_MIME_TYPE_BYTES {
                resource.post_error(
                    0_u32,
                    format!("selection MIME type exceeds the {MAX_MIME_TYPE_BYTES}-byte limit"),
                );
                return;
            }
            if *count >= MAX_SOURCE_MIME_TYPES {
                resource.post_error(
                    0_u32,
                    format!(
                        "selection source exceeded the {MAX_SOURCE_MIME_TYPES}-MIME-type limit"
                    ),
                );
                return;
            }
            *count += 1;
        }
        <PrimarySelectionState as Dispatch<
            ZwpPrimarySelectionSourceV1,
            PrimarySourceUserData,
            Self,
        >>::request(state, client, resource, request, data, display, data_init);
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &ZwpPrimarySelectionSourceV1,
        data: &PrimarySourceUserData,
    ) {
        state.selection_mime_counts.remove(&resource.id());
        <PrimarySelectionState as Dispatch<
            ZwpPrimarySelectionSourceV1,
            PrimarySourceUserData,
            Self,
        >>::destroyed(state, client, resource, data);
    }
}

delegate_compositor!(Compositor);
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    WlDataDeviceManager: ()
] => DataDeviceState);
delegate_shm!(Compositor);
delegate_dmabuf!(Compositor);
delegate_drm_syncobj!(Compositor);
delegate_fractional_scale!(Compositor);
delegate_output!(Compositor);
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    wp_presentation::WpPresentation: u32
] => PresentationState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    wp_presentation_feedback::WpPresentationFeedback: ()
] => PresentationFeedbackState);
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    ZwpKeyboardShortcutsInhibitManagerV1: ()
] => KeyboardShortcutsInhibitState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpKeyboardShortcutsInhibitorV1: KeyboardShortcutsInhibitorUserData
] => KeyboardShortcutsInhibitState);
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    ZwpRelativePointerManagerV1: ()
] => RelativePointerManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpRelativePointerV1: RelativePointerUserData<Self>
] => RelativePointerManagerState);
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    ZwpPointerConstraintsV1: ()
] => PointerConstraintsState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpConfinedPointerV1: PointerConstraintUserData<Self>
] => PointerConstraintsState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpLockedPointerV1: PointerConstraintUserData<Self>
] => PointerConstraintsState);
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    ZwpPointerGesturesV1: ()
] => PointerGesturesState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpPointerGestureSwipeV1: PointerGestureUserData<Self>
] => PointerGesturesState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpPointerGesturePinchV1: PointerGestureUserData<Self>
] => PointerGesturesState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpPointerGestureHoldV1: PointerGestureUserData<Self>
] => PointerGesturesState);
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    WpCursorShapeManagerV1: ()
] => CursorShapeManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    WpCursorShapeDeviceV1: CursorShapeDeviceUserData<Self>
] => CursorShapeManagerState);
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    ZwpTabletManagerV2: ()
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpTabletSeatV2: TabletSeatUserData
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpTabletToolV2: TabletToolUserData
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ZwpTabletV2: TabletUserData
] => TabletManagerState);
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    ZwpPrimarySelectionDeviceManagerV1: PrimaryDeviceManagerGlobalData
] => PrimarySelectionState);
delegate_xdg_shell!(Compositor);
delegate_xdg_decoration!(Compositor);
delegate_foreign_toplevel_list!(Compositor);
delegate_layer_shell!(Compositor);
delegate_xdg_activation!(Compositor);
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    WlSeat: SeatGlobalData<Compositor>
] => SeatState<Compositor>);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    WlPointer: PointerUserData<Compositor>
] => SeatState<Compositor>);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    WlKeyboard: KeyboardUserData<Compositor>
] => SeatState<Compositor>);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    WlTouch: TouchUserData<Compositor>
] => SeatState<Compositor>);
delegate_viewporter!(Compositor);

struct WaylandClientState {
    compositor_state: CompositorClientState,
    disconnected: Arc<AtomicUsize>,
    surface_count: Arc<AtomicUsize>,
    selection_source_count: Arc<AtomicUsize>,
    selection_device_count: Arc<AtomicUsize>,
    pointer_extension_count: Arc<AtomicUsize>,
    pointer_gesture_count: Arc<AtomicUsize>,
    cursor_shape_count: Arc<AtomicUsize>,
    touch_device_count: Arc<AtomicUsize>,
    tablet_seat_count: Arc<AtomicUsize>,
    presentation_feedback_count: Arc<AtomicUsize>,
    shortcut_inhibitor_count: Arc<AtomicUsize>,
    disconnected_client_ids: Arc<Mutex<VecDeque<ClientId>>>,
}

impl ClientData for WaylandClientState {
    fn initialized(&self, _client_id: ClientId) {}

    fn disconnected(&self, client_id: ClientId, _reason: DisconnectReason) {
        self.disconnected.fetch_add(1, Ordering::AcqRel);
        self.disconnected_client_ids
            .lock()
            .unwrap()
            .push_back(client_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_output(name: &str) -> Output {
        Output::new(
            name.to_owned(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Nobox".to_owned(),
                model: "Test output".to_owned(),
            },
        )
    }

    fn decorated_client() -> PolicyClient {
        let policy = ClientPolicy::for_role(ClientRole::Normal);
        PolicyClient {
            id: PolicyClientId::new(7),
            geometry: Geometry::new(100, 100, 300, 200),
            size_hints: SizeHints::default(),
            gravity: Gravity::default(),
            policy,
            natural_decorations: policy.decorations,
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
    fn socket_name_is_one_bounded_component() {
        assert!(validate_socket_name("nobox-wayland-test").is_ok());
        assert!(validate_socket_name("").is_err());
        assert!(validate_socket_name("../wayland-0").is_err());
        assert!(validate_socket_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn compositor_output_selection_and_pointer_confinement_handle_layout_gaps() {
        let mut display = Display::<Compositor>::new().unwrap();
        let left = test_output("left");
        let right = test_output("right");
        left.change_current_state(
            Some(OutputMode {
                size: (800, 600).into(),
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        right.change_current_state(
            Some(OutputMode {
                size: (1024, 768).into(),
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        let compositor = Compositor::new_with_outputs(
            &display.handle(),
            vec![
                CompositorOutput {
                    output: left.clone(),
                    geometry: Geometry::new(-800, 0, 800, 600),
                    primary: false,
                    global: None,
                },
                CompositorOutput {
                    output: right.clone(),
                    geometry: Geometry::new(200, 100, 1024, 768),
                    primary: true,
                    global: None,
                },
            ],
            Config::default(),
            OsString::from("wayland-test"),
            SessionRestore::default(),
        );

        assert_eq!(compositor.primary_output().output.name(), "right");
        assert_eq!(
            compositor
                .output_for_point((-400.0, 300.0).into())
                .output
                .name(),
            "left"
        );
        assert_eq!(
            compositor
                .output_for_point((400.0, 300.0).into())
                .output
                .name(),
            "right"
        );
        assert_eq!(
            compositor
                .output_for_point((100.0, 10.0).into())
                .output
                .name(),
            "right"
        );
        assert_eq!(
            compositor
                .output_for_geometry(Geometry::new(-50, 100, 300, 100))
                .output
                .name(),
            "right"
        );
        assert_eq!(
            compositor.clamp_point_to_outputs((100.0, 10.0).into()),
            (-1.0, 10.0).into()
        );
        assert_eq!(
            compositor.clamp_point_to_outputs((-900.0, 700.0).into()),
            (-800.0, 599.0).into()
        );

        let mut compositor = compositor;
        compositor.pointer_location = (-400.0, 300.0).into();
        compositor.replace_outputs(vec![CompositorOutput {
            output: right.clone(),
            geometry: Geometry::new(200, 100, 1024, 768),
            primary: true,
            global: None,
        }]);
        assert!(compositor.space.output_geometry(&left).is_none());
        assert_eq!(
            compositor.space.output_geometry(&right).unwrap().loc,
            (200, 100).into()
        );
        assert_eq!(compositor.pointer_location, (200.0, 300.0).into());

        drop(compositor);
        display.flush_clients().unwrap();
    }

    #[test]
    fn compositor_chooses_a_primary_output_when_configuration_omits_one() {
        let display = Display::<Compositor>::new().unwrap();
        let compositor = Compositor::new_with_outputs(
            &display.handle(),
            vec![CompositorOutput {
                output: test_output("only"),
                geometry: Geometry::new(0, 0, 640, 480),
                primary: false,
                global: None,
            }],
            Config::default(),
            OsString::from("wayland-test"),
            SessionRestore::default(),
        );

        assert_eq!(compositor.primary_output().output.name(), "only");
        assert!(compositor.primary_output().primary);
    }

    #[test]
    fn layer_exclusive_zone_and_config_margins_share_core_work_area_policy() {
        let work_area = work_area_from_nonexclusive_zone(
            Geometry::new(0, 0, 800, 600),
            Rectangle::new((0, 32).into(), (800, 568).into()),
            MarginConfig {
                left: 10,
                right: 20,
                top: 5,
                bottom: 7,
            },
        );
        assert_eq!(work_area, Geometry::new(10, 37, 770, 556));
    }

    #[test]
    fn native_application_geometry_honors_outer_size_and_axis_gravity() {
        let half = nobox_config::PositiveRelativeAmount::try_from(
            "50%".parse::<nobox_config::RelativeAmount>().unwrap(),
        )
        .unwrap();
        assert_eq!(
            requested_application_dimension(Some(half), SizeBasis::Outer, 800, 160, 10),
            390
        );
        assert_eq!(
            placed_application_axis(
                AxisPosition::End(nobox_config::RelativeAmount::Pixels(10)),
                20,
                800,
                200,
            ),
            610
        );
    }

    #[test]
    fn wayland_binding_input_matches_canonical_modifiers_and_raw_symbol() {
        let input = BindingInput {
            modifiers: vec![KeyboardModifier::Shift, KeyboardModifier::Super],
            symbols: vec!["Q".to_owned(), "q".to_owned()],
        };
        let chord = "W-S-q".parse::<KeyChord>().unwrap();
        assert!(input.matches(&chord));
        assert!(!input.matches(&"W-q".parse::<KeyChord>().unwrap()));
    }

    #[test]
    fn keyboard_interactive_keys_keep_direction_and_modifiers_protocol_neutral() {
        let input = BindingInput {
            modifiers: vec![KeyboardModifier::Control, KeyboardModifier::Shift],
            symbols: vec!["Left".to_owned()],
        };
        assert_eq!(
            binding_cardinal_direction(&input),
            Some(CardinalDirection::Left)
        );
        assert!(cardinal_directions_share_axis(
            CardinalDirection::Left,
            CardinalDirection::Right
        ));
        assert!(!cardinal_directions_share_axis(
            CardinalDirection::Left,
            CardinalDirection::Up
        ));
        assert_eq!(
            cardinal_direction_delta(CardinalDirection::Left, 8),
            (-8, 0)
        );
    }

    #[test]
    fn wayland_key_sequences_intercept_prefix_complete_and_quit() {
        let config = Config::parse(
            "[keyboard]\ninherit_defaults = false\nchain_quit_key = 'C-g'\n\
             [[keyboard.bindings]]\nkey = 'W-x C-s'\naction = { type = 'debug', message = 'saved' }",
        )
        .unwrap();
        let input = |modifiers, symbol: &str| BindingInput {
            modifiers,
            symbols: vec![symbol.to_owned()],
        };
        let mut chain = None;

        assert!(matches!(
            resolve_configured_binding(
                &config,
                &mut chain,
                &input(vec![KeyboardModifier::Super], "x")
            ),
            BindingOutcome::Intercept(actions) if actions.is_empty()
        ));
        assert_eq!(chain.as_ref().map(|chain| chain.depth), Some(1));
        assert!(matches!(
            resolve_configured_binding(&config, &mut chain, &input(Vec::new(), "z")),
            BindingOutcome::Forward
        ));
        assert!(matches!(
            resolve_configured_binding(
                &config,
                &mut chain,
                &input(vec![KeyboardModifier::Control], "g")
            ),
            BindingOutcome::Intercept(actions) if actions.is_empty()
        ));
        assert!(chain.is_none());

        let _ = resolve_configured_binding(
            &config,
            &mut chain,
            &input(vec![KeyboardModifier::Super], "x"),
        );
        assert!(matches!(
            resolve_configured_binding(
                &config,
                &mut chain,
                &input(vec![KeyboardModifier::Control], "s")
            ),
            BindingOutcome::Intercept(actions)
                if matches!(actions.as_slice(), [Action::Debug { message }] if message == "saved")
        ));
        assert!(chain.is_none());
    }

    #[test]
    fn wayland_frame_hit_testing_matches_shared_mouse_contexts() {
        let client = decorated_client();
        let config = Config::default();
        let extents = client
            .policy
            .decorations
            .extents(config.theme.border_width, config.theme.titlebar_height);

        assert_eq!(
            frame_context_at(client, extents, (110.0, 75.0).into()),
            MouseContext::Top
        );
        assert_eq!(
            frame_context_at(client, extents, (99.0, 75.0).into()),
            MouseContext::TopLeft
        );
        assert_eq!(
            frame_context_at(client, extents, (99.0, 150.0).into()),
            MouseContext::Left
        );
        assert_eq!(
            frame_context_at(client, extents, (110.0, 80.0).into()),
            MouseContext::Titlebar
        );
        assert_eq!(
            frame_context_at(client, extents, (150.0, 150.0).into()),
            MouseContext::Client
        );
        assert_eq!(
            frame_context_at(client, extents, (400.0, 300.0).into()),
            MouseContext::BottomRight
        );
        assert_eq!(
            mouse_context_chain(MouseContext::TopLeft),
            &[
                MouseContext::TopLeft,
                MouseContext::Border,
                MouseContext::Frame,
            ]
        );
    }

    #[test]
    fn titlebar_button_rendering_and_hit_geometry_share_rectangles() {
        let client = decorated_client();
        let buttons = frame_button_geometries(client, &Config::default());
        assert_eq!(
            buttons
                .iter()
                .map(|(button, _)| *button)
                .collect::<Vec<_>>(),
            [
                FrameButton::Close,
                FrameButton::Maximize,
                FrameButton::Minimize,
            ]
        );
        assert!(buttons.windows(2).all(|pair| pair[0].1.x > pair[1].1.x));
        for (button, geometry) in buttons {
            assert!(!frame_button_glyph(button, geometry).is_empty());
            assert!(geometry_contains_point(
                geometry,
                (
                    f64::from(geometry.x) + f64::from(geometry.width) / 2.0,
                    f64::from(geometry.y) + f64::from(geometry.height) / 2.0,
                )
                    .into()
            ));
        }
        assert_eq!(pointer_button_number(0x110), Some(1));
        assert_eq!(pointer_button_number(0x111), Some(3));
        assert_eq!(pointer_button_number(0x120), None);
    }

    #[test]
    fn compositor_text_layout_respects_alignment_and_clipping() {
        let bounds = Geometry::new(10, 20, 100, 24);
        assert_eq!(text_origin(bounds, 40, TitleAlignment::Left), 10);
        assert_eq!(text_origin(bounds, 40, TitleAlignment::Center), 40);
        assert_eq!(text_origin(bounds, 40, TitleAlignment::Right), 70);
        assert_eq!(text_origin(bounds, 140, TitleAlignment::Right), 10);
        assert_eq!(
            horizontal_inset(bounds, 8, 12),
            Some(Geometry::new(18, 20, 80, 24))
        );
        assert_eq!(horizontal_inset(bounds, 50, 50), None);
    }

    #[test]
    fn focus_switcher_window_and_modifier_bounds_are_deterministic() {
        assert_eq!(focus_cycle_visible_start(3, 2, 8), 0);
        assert_eq!(focus_cycle_visible_start(10, 0, 4), 0);
        assert_eq!(focus_cycle_visible_start(10, 5, 4), 3);
        assert_eq!(focus_cycle_visible_start(10, 9, 4), 6);
        assert_eq!(focus_cycle_visible_start(10, 9, 0), 0);
        assert_eq!(
            focus_cycle_modifiers(&[KeyboardModifier::Alt, KeyboardModifier::Shift]),
            [KeyboardModifier::Alt]
        );
        assert_eq!(
            focus_cycle_modifiers(&[KeyboardModifier::Shift]),
            [KeyboardModifier::Shift]
        );
        assert_eq!(centered_axis(-100, 800, 420), 90);
        assert_eq!(
            outline_geometries(Geometry::new(10, 20, 100, 50), 3),
            [
                Geometry::new(10, 20, 100, 3),
                Geometry::new(10, 67, 100, 3),
                Geometry::new(10, 20, 3, 50),
                Geometry::new(107, 20, 3, 50),
            ]
        );
        assert_eq!(
            fallback_cursor_geometries((10, 20).into()),
            [
                Geometry::new(10, 20, 2, 16),
                Geometry::new(12, 22, 2, 12),
                Geometry::new(14, 24, 2, 8),
                Geometry::new(16, 26, 2, 4),
                Geometry::new(14, 32, 2, 4),
                Geometry::new(16, 36, 2, 4),
            ]
        );
        let location = Point::<i32, Logical>::from((100, 100));
        let default_cursor = named_cursor_geometries(CursorIcon::Default, location);
        let text_cursor = named_cursor_geometries(CursorIcon::Text, location);
        let resize_cursor = named_cursor_geometries(CursorIcon::EwResize, location);
        assert_ne!(default_cursor, text_cursor);
        assert_ne!(text_cursor, resize_cursor);
        assert_ne!(default_cursor, resize_cursor);
        for icon in [
            CursorIcon::Default,
            CursorIcon::ContextMenu,
            CursorIcon::Help,
            CursorIcon::Pointer,
            CursorIcon::Progress,
            CursorIcon::Wait,
            CursorIcon::Cell,
            CursorIcon::Crosshair,
            CursorIcon::Text,
            CursorIcon::VerticalText,
            CursorIcon::Alias,
            CursorIcon::Copy,
            CursorIcon::Move,
            CursorIcon::NoDrop,
            CursorIcon::NotAllowed,
            CursorIcon::Grab,
            CursorIcon::Grabbing,
            CursorIcon::EResize,
            CursorIcon::NResize,
            CursorIcon::NeResize,
            CursorIcon::NwResize,
            CursorIcon::SResize,
            CursorIcon::SeResize,
            CursorIcon::SwResize,
            CursorIcon::WResize,
            CursorIcon::EwResize,
            CursorIcon::NsResize,
            CursorIcon::NeswResize,
            CursorIcon::NwseResize,
            CursorIcon::ColResize,
            CursorIcon::RowResize,
            CursorIcon::AllScroll,
            CursorIcon::ZoomIn,
            CursorIcon::ZoomOut,
            CursorIcon::DndAsk,
            CursorIcon::AllResize,
        ] {
            let geometries = named_cursor_geometries(icon, location);
            assert!(!geometries.is_empty(), "{icon:?} has no cursor geometry");
            assert!(geometries.len() <= 16, "{icon:?} cursor is not bounded");
            assert!(
                geometries
                    .iter()
                    .all(|geometry| geometry.width > 0 && geometry.height > 0),
                "{icon:?} cursor contains empty geometry"
            );
        }
    }

    #[test]
    fn bounded_reservations_never_exceed_limit_and_can_be_reused() {
        let counter = AtomicUsize::new(0);
        assert!(reserve_bounded(&counter, 2));
        assert!(reserve_bounded(&counter, 2));
        assert!(!reserve_bounded(&counter, 2));
        assert_eq!(counter.load(Ordering::Acquire), 2);
        release_reservation(&counter);
        assert!(reserve_bounded(&counter, 2));
        assert_eq!(counter.load(Ordering::Acquire), 2);
    }
}
