//! Experimental native Wayland compositor infrastructure.
//!
//! Milestone W0 intentionally exposes only compositor, shared-memory, and
//! output globals. It does not manage application surfaces.

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
    time::Duration,
};

use nobox_runtime::{BackendKind, ControlRequest, ControlSender, ControlServer};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind as _, Color32F, ExportMem as _, Frame as _, Offscreen as _, Renderer as _,
            pixman::PixmanRenderer,
        },
    },
    delegate_compositor, delegate_output, delegate_shm,
    output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            EventLoop, Interest, Mode, PostAction,
            channel::{self, Event as ChannelEvent},
            generic::Generic,
        },
        wayland_server::{
            Client, Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_surface::WlSurface},
        },
    },
    utils::{Rectangle, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        output::{OutputHandler, OutputManagerState},
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
    },
};
use thiserror::Error;
use tracing::{info, warn};
use x11rb::{
    connection::Connection as _,
    protocol::xproto::{
        ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, ImageFormat, WindowClass,
    },
    rust_connection::RustConnection,
};

/// Exact Smithay release selected for the experimental backend.
pub const SMITHAY_VERSION: &str = "0.7.0";

/// Configuration for the nested-X11 infrastructure proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NestedOptions {
    /// Parent X11 display; `None` uses `DISPLAY`.
    pub display: Option<String>,
    /// Name to create below `XDG_RUNTIME_DIR`.
    pub socket_name: String,
    /// Stop after this many clients have disconnected; zero runs until closed.
    pub exit_after_disconnects: usize,
}

impl Default for NestedOptions {
    fn default() -> Self {
        Self {
            display: None,
            socket_name: format!("nobox-w0-{}", std::process::id()),
            exit_after_disconnects: 0,
        }
    }
}

/// Environment diagnostics for the nested-X11 proof backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NestedDiagnostics {
    /// Selected X11 display.
    pub display: String,
    /// Private runtime directory that will contain the Wayland socket.
    pub runtime_dir: PathBuf,
}

impl NestedDiagnostics {
    /// Validate the process environment needed by the W0 nested backend.
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
}

/// Errors produced by the experimental Wayland infrastructure.
#[derive(Debug, Error)]
pub enum WaylandError {
    /// No nested X server was selected.
    #[error("DISPLAY is unset; the W0 backend requires an isolated nested X server")]
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

/// Run the W0 compositor proof in a software-rendered window on the selected X11 display.
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
    let compositor = Compositor::new(&display_handle);
    let nested_window = NestedX11Window::create(options.display.as_deref())?;
    let mode = OutputMode {
        size: nested_window.size,
        refresh: 60_000,
    };
    let output = Output::new(
        "nobox-w0".to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Nobox".to_owned(),
            model: "W0 synthetic output".to_owned(),
        },
    );
    let _global = output.create_global::<Compositor>(&display_handle);
    output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
    output.set_preferred(mode);
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
                        warn!("Wayland skeleton has no managed session state to save yet");
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
            let client_data = Arc::new(W0ClientState {
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
        display
            .flush_clients()
            .map_err(|error| WaylandError::EventLoop(error.to_string()))?;
        if std::mem::take(&mut data.reload_requested) {
            info!("Wayland skeleton received a configuration reload request");
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

    drop(nested_window);

    Ok(RunReport {
        socket_name,
        rendered_frames: data.rendered_frames,
        disconnected_clients: disconnected.load(Ordering::Acquire),
    })
}

struct NestedX11Window {
    _connection: RustConnection,
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
                    .event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY),
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
            _connection: connection,
            size: physical_size,
        })
    }
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
    _output_manager_state: OutputManagerState,
}

impl Compositor {
    fn new(display: &DisplayHandle) -> Self {
        Self {
            compositor_state: CompositorState::new::<Self>(display),
            shm_state: ShmState::new::<Self>(display, Vec::new()),
            _output_manager_state: OutputManagerState::new(),
        }
    }
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
            .get_data::<W0ClientState>()
            .expect("all W0 clients are inserted with W0ClientState")
            .compositor_state
    }

    fn commit(&mut self, _surface: &WlSurface) {}
}

impl ShmHandler for Compositor {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl OutputHandler for Compositor {}

delegate_compositor!(Compositor);
delegate_shm!(Compositor);
delegate_output!(Compositor);

struct W0ClientState {
    compositor_state: CompositorClientState,
    disconnected: Arc<AtomicUsize>,
}

impl ClientData for W0ClientState {
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
        assert!(validate_socket_name("nobox-w0-test").is_ok());
        assert!(validate_socket_name("").is_err());
        assert!(validate_socket_name("../wayland-0").is_err());
        assert!(validate_socket_name(&"x".repeat(65)).is_err());
    }
}
