//! Native Wayland compositor backend, currently available as a managed nested shell.
//!
//! The backend owns Wayland protocol translation and rendering while window
//! management decisions remain in `nobox-core`.

use std::{
    ffi::OsString,
    fs,
    os::{
        fd::AsFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use nobox_core::{
    Client as PolicyClient, ClientId as PolicyClientId, ClientLayer, ClientPolicy,
    ClientPresentation, ClientRole, ClientSet, DecorationOverride, Geometry, Gravity, ResizeDeltas,
    Size, SizeHints, WorkspaceAssignment, relative_resize_geometry,
};
use nobox_runtime::{BackendKind, ControlRequest, ControlSender, ControlServer};
use smithay::{
    backend::{
        allocator::Fourcc,
        input::{
            AbsolutePositionEvent as _, ButtonState, Event as _, InputEvent, KeyState,
            KeyboardKeyEvent as _, PointerButtonEvent as _,
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
    delegate_compositor, delegate_output, delegate_seat, delegate_shm, delegate_xdg_decoration,
    delegate_xdg_shell,
    desktop::{
        PopupKeyboardGrab, PopupManager, PopupPointerGrab, Space, Window, WindowSurfaceType,
        find_popup_root_surface,
    },
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, Keycode, keysyms},
        pointer::{ButtonEvent, CursorImageStatus, CursorImageSurfaceData, Focus, MotionEvent},
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
            Client, Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Serial, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
            TraversalAction, with_states, with_surface_tree_downward,
        },
        output::{OutputHandler, OutputManagerState},
        seat::WaylandFocus,
        shell::xdg::{
            PopupSurface, PositionerState, ShellClient,
            SurfaceCachedState as XdgSurfaceCachedState, ToplevelSurface, XdgShellHandler,
            XdgShellState,
            decoration::{XdgDecorationHandler, XdgDecorationState},
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
    },
};
use thiserror::Error;
use tracing::{info, warn};
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
    /// Smithay, X11, or calloop could not initialize the proof backend.
    #[error("could not initialize nested Wayland backend: {0}")]
    Initialization(String),
    /// The compositor event loop failed.
    #[error("nested Wayland event loop failed: {0}")]
    EventLoop(String),
    /// The clear-color renderer failed.
    #[error("nested Wayland renderer failed: {0}")]
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
    let compositor = Compositor::new(&display_handle, output, nested_window.size());
    let mut data = LoopData {
        compositor,
        display_handle: display_handle.clone(),
        display_ready: false,
        rendered_frames: 1,
        fatal_error: None,
        running: true,
        reload_requested: false,
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
                    ControlRequest::SaveSession => {
                        warn!("Wayland shell has no managed session state to save yet");
                    }
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
            let client_data = Arc::new(WaylandClientState {
                compositor_state: CompositorClientState::default(),
                disconnected: Arc::clone(&client_disconnects),
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
        nested_window.dispatch_input(&mut data.compositor)?;
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

    Ok(RunReport {
        socket_name,
        rendered_frames: data.rendered_frames,
        disconnected_clients: disconnected.load(Ordering::Acquire),
        renderer,
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
            Key(Keycode, KeyState, u32),
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
                WinitEvent::Input(InputEvent::Keyboard { event }) => events.push(Event::Key(
                    event.key_code(),
                    event.state(),
                    event.time_msec(),
                )),
                WinitEvent::CloseRequested => events.push(Event::Close),
                WinitEvent::Resized { size, .. } => events.push(Event::Resize(size)),
                WinitEvent::Redraw => events.push(Event::Redraw),
                _ => {}
            });
        for event in events {
            match event {
                Event::Motion(x, y, time) => compositor.pointer_motion(x, y, time),
                Event::Button(button, state, time) => {
                    compositor.pointer_button_code(button, state, time);
                }
                Event::Key(key, state, time) => compositor.keyboard_keycode(key, state, time),
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
        compositor.space.refresh();
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
            let decorations = compositor.decoration_elements();
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
        compositor.space.refresh();
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
        let decorations = compositor.decoration_elements();
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
                X11Event::MotionNotify(event) => compositor.pointer_motion(
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

fn send_frame_callbacks(surface: &WlSurface, time: u32) {
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
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
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

struct LoopData {
    compositor: Compositor,
    display_handle: DisplayHandle,
    display_ready: bool,
    rendered_frames: usize,
    fatal_error: Option<String>,
    running: bool,
    reload_requested: bool,
    runtime_control: Option<ControlServer>,
}

impl LoopData {
    fn fail(&mut self, error: String) {
        self.fatal_error = Some(error);
    }
}

struct Compositor {
    compositor_state: CompositorState,
    shm_state: ShmState,
    xdg_shell_state: XdgShellState,
    _xdg_decoration_state: XdgDecorationState,
    seat_state: SeatState<Self>,
    seat: Seat<Self>,
    _output_manager_state: OutputManagerState,
    output: Output,
    output_geometry: Geometry,
    popup_manager: PopupManager,
    space: Space<Window>,
    clients: ClientSet,
    windows: Vec<ManagedWindow>,
    next_client_id: u64,
    pointer_location: Point<f64, Logical>,
    cursor_status: CursorImageStatus,
    interactive: Option<InteractiveOperation>,
    redraw_needed: bool,
    exit_requested: bool,
    started: Instant,
}

impl Compositor {
    fn new(
        display: &DisplayHandle,
        output: Output,
        size: smithay::utils::Size<i32, smithay::utils::Physical>,
    ) -> Self {
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(display, "nobox");
        let _keyboard = seat
            .add_keyboard(Default::default(), 250, 25)
            .expect("the built-in keyboard configuration is valid");
        let _pointer = seat.add_pointer();
        let mut space = Space::default();
        space.map_output(&output, (0, 0));
        Self {
            compositor_state: CompositorState::new::<Self>(display),
            shm_state: ShmState::new::<Self>(display, Vec::new()),
            xdg_shell_state: XdgShellState::new::<Self>(display),
            _xdg_decoration_state: XdgDecorationState::new::<Self>(display),
            seat_state,
            seat,
            _output_manager_state: OutputManagerState::new(),
            output,
            output_geometry: Geometry::new(
                0,
                0,
                u32::try_from(size.w).unwrap_or(1),
                u32::try_from(size.h).unwrap_or(1),
            ),
            popup_manager: PopupManager::default(),
            space,
            clients: ClientSet::default(),
            windows: Vec::new(),
            next_client_id: 1,
            pointer_location: (0.0, 0.0).into(),
            cursor_status: CursorImageStatus::default_named(),
            interactive: None,
            redraw_needed: true,
            exit_requested: false,
            started: Instant::now(),
        }
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

    fn resize_output(&mut self, size: smithay::utils::Size<i32, Physical>) {
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        self.output_geometry.width = u32::try_from(size.w).unwrap_or(1);
        self.output_geometry.height = u32::try_from(size.h).unwrap_or(1);
        let mode = OutputMode {
            size,
            refresh: 60_000,
        };
        self.output
            .change_current_state(Some(mode), None, None, None);
        self.output.set_preferred(mode);
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
        for managed in &self.windows {
            if let Some(surface) = managed.window.wl_surface() {
                send_frame_callbacks(&surface, elapsed);
                let popups = PopupManager::popups_for_surface(&surface)
                    .map(|(popup, _)| popup.wl_surface().clone())
                    .collect::<Vec<_>>();
                for popup in popups {
                    send_frame_callbacks(&popup, elapsed);
                }
            }
        }
        self.redraw_needed = false;
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
            self.space.unmap_elem(&window);
            let _ = self.clients.unmanage(id);
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
        if !self.clients.contains(id) {
            let offset = i32::try_from((self.clients.len() % 8) * 28).unwrap_or(0);
            let placed = Geometry::new(32 + offset, 32 + offset, width, height);
            let policy = ClientPolicy::for_role(ClientRole::Normal);
            let _ = self.clients.manage(PolicyClient {
                id,
                geometry: placed,
                size_hints,
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
            });
            let _ = self.clients.focus(id);
            let _ = self.clients.raise(id);
            self.sync_focus_and_stacking();
        } else if let Some(current) = self.clients.get(id).copied() {
            let _ = self.clients.set_size_hints(id, size_hints);
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
        if let Some(id) = focused
            && let Some(surface) = self
                .windows
                .iter()
                .find(|window| window.id == id)
                .and_then(|window| window.window.wl_surface())
        {
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.set_focus(self, Some(surface.into_owned()), Serial::from(0));
            }
        }
        for managed in &self.windows {
            if managed.window.set_activated(focused == Some(managed.id)) {
                if let Some(toplevel) = managed.window.toplevel() {
                    toplevel.send_pending_configure();
                }
            }
        }
    }

    fn pointer_motion(&mut self, x: f64, y: f64, time: u32) {
        let location = (x, y).into();
        self.pointer_location = location;
        self.update_interactive(location);
        let focus = self
            .space
            .element_under(location)
            .and_then(|(window, window_location)| {
                window
                    .surface_under(location - window_location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, surface_location)| {
                        (surface, (window_location + surface_location).to_f64())
                    })
            });
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.motion(
                self,
                focus,
                &MotionEvent {
                    location,
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
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

    fn pointer_button_code(&mut self, button: u32, state: ButtonState, time: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if state == ButtonState::Pressed
            && let Some(surface) = pointer.current_focus()
            && let Some(id) = self.surface_window(&surface).map(|window| window.id)
        {
            let _ = self.clients.focus(id);
            let _ = self.clients.raise(id);
            self.sync_focus_and_stacking();
        }
        pointer.button(
            self,
            &ButtonEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                button,
                state,
            },
        );
        if state == ButtonState::Released {
            self.finish_interactive();
        }
        self.redraw_needed = true;
    }

    fn update_interactive(&mut self, location: Point<f64, Logical>) {
        let Some(operation) = self.interactive else {
            return;
        };
        let dx = (location.x - operation.start_pointer.x).round() as i32;
        let dy = (location.y - operation.start_pointer.y).round() as i32;
        let geometry = match operation.kind {
            InteractiveKind::Move => Geometry::new(
                operation.start_geometry.x.saturating_add(dx),
                operation.start_geometry.y.saturating_add(dy),
                operation.start_geometry.width,
                operation.start_geometry.height,
            ),
            InteractiveKind::Resize(edges) => {
                let left = matches!(
                    edges,
                    xdg_toplevel::ResizeEdge::TopLeft
                        | xdg_toplevel::ResizeEdge::BottomLeft
                        | xdg_toplevel::ResizeEdge::Left
                );
                let right = matches!(
                    edges,
                    xdg_toplevel::ResizeEdge::TopRight
                        | xdg_toplevel::ResizeEdge::BottomRight
                        | xdg_toplevel::ResizeEdge::Right
                );
                let top = matches!(
                    edges,
                    xdg_toplevel::ResizeEdge::TopLeft
                        | xdg_toplevel::ResizeEdge::Top
                        | xdg_toplevel::ResizeEdge::TopRight
                );
                let bottom = matches!(
                    edges,
                    xdg_toplevel::ResizeEdge::BottomLeft
                        | xdg_toplevel::ResizeEdge::Bottom
                        | xdg_toplevel::ResizeEdge::BottomRight
                );
                let Some(client) = self.clients.get(operation.id) else {
                    return;
                };
                relative_resize_geometry(
                    operation.start_geometry,
                    ResizeDeltas {
                        left: if left { dx.saturating_neg() } else { 0 },
                        right: if right { dx } else { 0 },
                        top: if top { dy.saturating_neg() } else { 0 },
                        bottom: if bottom { dy } else { 0 },
                    },
                    client.size_hints,
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

    fn keyboard_key(&mut self, detail: u8, state: KeyState, time: u32) {
        self.keyboard_keycode(Keycode::new(u32::from(detail)), state, time);
    }

    fn keyboard_keycode(&mut self, keycode: Keycode, state: KeyState, time: u32) {
        if let Some(keyboard) = self.seat.get_keyboard() {
            let close = keyboard.input::<bool, _>(
                self,
                keycode,
                state,
                SERIAL_COUNTER.next_serial(),
                time,
                |_, _, key| {
                    if key.modified_sym().raw() == keysyms::KEY_Escape {
                        FilterResult::Intercept(true)
                    } else {
                        FilterResult::Forward
                    }
                },
            ) == Some(true);
            if close
                && state == KeyState::Pressed
                && let Some(id) = self.clients.focused()
                && let Some(toplevel) = self
                    .windows
                    .iter()
                    .find(|managed| managed.id == id)
                    .and_then(|managed| managed.window.toplevel())
            {
                toplevel.send_close();
            }
        }
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
            let border_color = if focused {
                [0.20, 0.48, 0.82, 1.0]
            } else {
                [0.25, 0.28, 0.34, 1.0]
            };
            let title_color = if focused {
                [0.12, 0.30, 0.58, 1.0]
            } else {
                [0.16, 0.18, 0.23, 1.0]
            };
            let border = SolidColorBuffer::new(
                (width.saturating_add(4), height.saturating_add(28)),
                border_color,
            );
            elements.push(SolidColorRenderElement::from_buffer(
                &border,
                (
                    client.geometry.x.saturating_sub(2),
                    client.geometry.y.saturating_sub(26),
                ),
                1.0,
                1.0,
                Kind::Unspecified,
            ));
            let title = SolidColorBuffer::new((width, 24), title_color);
            elements.push(SolidColorRenderElement::from_buffer(
                &title,
                (client.geometry.x, client.geometry.y.saturating_sub(24)),
                1.0,
                1.0,
                Kind::Unspecified,
            ));
        }
        if matches!(self.cursor_status, CursorImageStatus::Named(_)) {
            let location = (
                self.pointer_location.x.round() as i32,
                self.pointer_location.y.round() as i32,
            );
            let color = [0.92, 0.94, 0.98, 1.0];
            let stem = SolidColorBuffer::new((2, 14), color);
            elements.push(SolidColorRenderElement::from_buffer(
                &stem,
                location,
                1.0,
                1.0,
                Kind::Cursor,
            ));
            let head = SolidColorBuffer::new((8, 2), color);
            elements.push(SolidColorRenderElement::from_buffer(
                &head,
                location,
                1.0,
                1.0,
                Kind::Cursor,
            ));
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
    last_ping: Instant,
    pending_ping: Option<(Serial, Instant)>,
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

impl BufferHandler for Compositor {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
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

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.popup_manager.commit(surface);
        self.map_toplevel_if_ready(surface);
        self.popup_manager.cleanup();
        self.redraw_needed = true;
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
                    i32::try_from(self.output_geometry.width).unwrap_or(i32::MAX),
                    i32::try_from(self.output_geometry.height).unwrap_or(i32::MAX),
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
            last_ping: Instant::now(),
            pending_ping: None,
        });
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let geometry = positioner.get_unconstrained_geometry(Rectangle::new(
            (0, 0).into(),
            (
                i32::try_from(self.output_geometry.width).unwrap_or(i32::MAX),
                i32::try_from(self.output_geometry.height).unwrap_or(i32::MAX),
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
                i32::try_from(self.output_geometry.width).unwrap_or(i32::MAX),
                i32::try_from(self.output_geometry.height).unwrap_or(i32::MAX),
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
        if let Some(geometry) = self
            .clients
            .set_maximized(id, true, true, self.output_geometry)
        {
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
            .set_maximized(id, false, false, self.output_geometry)
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
        if let Some(geometry) = self.clients.set_fullscreen(id, true, self.output_geometry) {
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
        if let Some(geometry) = self.clients.set_fullscreen(id, false, self.output_geometry) {
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
            let managed = self.windows.remove(index);
            self.space.unmap_elem(&managed.window);
            let _ = self.clients.unmanage(managed.id);
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

impl SeatHandler for Compositor {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&WlSurface>) {
        if let Some(id) =
            focused.and_then(|surface| self.surface_window(surface).map(|window| window.id))
        {
            let _ = self.clients.focus(id);
            let _ = self.clients.raise(id);
        }
        self.redraw_needed = true;
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
        self.redraw_needed = true;
    }
}

delegate_compositor!(Compositor);
delegate_shm!(Compositor);
delegate_output!(Compositor);
delegate_xdg_shell!(Compositor);
delegate_xdg_decoration!(Compositor);
delegate_seat!(Compositor);

struct WaylandClientState {
    compositor_state: CompositorClientState,
    disconnected: Arc<AtomicUsize>,
}

impl ClientData for WaylandClientState {
    fn initialized(&self, _client_id: ClientId) {}

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        self.disconnected.fetch_add(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_name_is_one_bounded_component() {
        assert!(validate_socket_name("nobox-wayland-test").is_ok());
        assert!(validate_socket_name("").is_err());
        assert!(validate_socket_name("../wayland-0").is_err());
        assert!(validate_socket_name(&"x".repeat(65)).is_err());
    }
}
