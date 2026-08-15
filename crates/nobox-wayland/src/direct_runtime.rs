//! Explicit direct-session compositor runtime for W4 hardware bring-up.

use std::{
    ffi::OsString,
    os::fd::AsFd as _,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use nobox_config::{Config, OutputTransform};
use nobox_runtime::{
    BackendKind, ControlRequest, ControlSender, ControlServer, RunDisposition, SessionRestore,
    SessionSnapshot,
};
use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
            compositor::{FrameFlags, PrimaryPlaneElement},
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::context::ContextPriority,
        input::{
            AbsolutePositionEvent as _, Axis, Event as _, InputEvent, KeyboardKeyEvent as _,
            PointerAxisEvent as _, PointerButtonEvent as _, PointerMotionEvent as _,
        },
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            Color32F, ImportAll, ImportMem,
            element::{
                Kind, render_elements,
                solid::SolidColorRenderElement,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
                utils::{Relocate, RelocateRenderElement},
            },
            gles::GlesRenderer,
            multigpu::{GpuManager, gbm::GbmGlesBackend},
        },
        session::{Event as SessionEvent, Session as _, libseat::LibSeatSession},
        udev::UdevBackend,
    },
    desktop::space::SpaceRenderElements,
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{
            EventLoop, Interest, Mode, PostAction,
            channel::{self, Event as ChannelEvent},
            generic::Generic,
        },
        drm::control::{ModeTypeFlags, crtc},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::{Display, DisplayHandle},
    },
    utils::{DeviceFd, Logical, Physical, Point, Transform},
    wayland::socket::ListeningSocketSource,
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};
use tracing::{info, warn};

use super::{
    Compositor, DirectConnector, DirectMode, DirectTopology, WaylandClientState, WaylandError,
    validate_socket_name,
};

type DirectGbmBackend = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
type DirectGpus = GpuManager<DirectGbmBackend>;
type DirectAllocator = GbmAllocator<DrmDeviceFd>;
type DirectExporter = GbmFramebufferExporter<DrmDeviceFd>;
type DirectDrmOutput = DrmOutput<DirectAllocator, DirectExporter, (), DrmDeviceFd>;
type DirectOutputManager = DrmOutputManager<DirectAllocator, DirectExporter, (), DrmDeviceFd>;

render_elements! {
    DirectRenderElement<R, E> where R: ImportAll + ImportMem;
    Space=SpaceRenderElements<R, E>,
    Surface=WaylandSurfaceRenderElement<R>,
    Solid=RelocateRenderElement<SolidColorRenderElement>,
}

impl<R, E> std::fmt::Debug for DirectRenderElement<R, E>
where
    R: smithay::backend::renderer::Renderer + ImportAll + ImportMem,
    E: smithay::backend::renderer::element::RenderElement<R> + std::fmt::Debug,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Space(element) => formatter.debug_tuple("Space").field(element).finish(),
            Self::Surface(element) => formatter.debug_tuple("Surface").field(element).finish(),
            Self::Solid(element) => formatter.debug_tuple("Solid").field(element).finish(),
            Self::_GenericCatcher(element) => formatter
                .debug_tuple("_GenericCatcher")
                .field(element)
                .finish(),
        }
    }
}

/// Configuration for an explicit unprivileged libseat/DRM session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectOptions {
    /// Name to create below the validated `XDG_RUNTIME_DIR`.
    pub socket_name: String,
    /// Stop after this many client disconnects; zero runs until requested exit.
    pub exit_after_disconnects: usize,
}

impl Default for DirectOptions {
    fn default() -> Self {
        Self {
            socket_name: format!("nobox-wayland-{}", std::process::id()),
            exit_after_disconnects: 0,
        }
    }
}

/// Result of a completed direct compositor run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectRunReport {
    /// Wayland socket that was served.
    pub socket_name: OsString,
    /// Number of KMS frames queued successfully.
    pub rendered_frames: usize,
    /// Number of client disconnects observed.
    pub disconnected_clients: usize,
    snapshot: SessionSnapshot,
    disposition: RunDisposition,
}

impl DirectRunReport {
    /// Separates neutral session state from the requested process action.
    #[must_use]
    pub fn into_parts(self) -> (SessionSnapshot, RunDisposition) {
        (self.snapshot, self.disposition)
    }
}

struct DirectBackend {
    session: LibSeatSession,
    libinput: Libinput,
    gpus: DirectGpus,
    node: DrmNode,
    render_node: DrmNode,
    crtc: crtc::Handle,
    output: Output,
    output_manager: DirectOutputManager,
    drm_output: DirectDrmOutput,
    active: bool,
    frame_pending: bool,
}

struct DirectLoopData {
    compositor: Compositor,
    display_handle: DisplayHandle,
    display_ready: bool,
    rendered_frames: usize,
    fatal_error: Option<String>,
    running: bool,
    reload_requested: bool,
    session_save_requested: bool,
    runtime_control: Option<ControlServer>,
    backend: DirectBackend,
}

impl DirectLoopData {
    fn fail(&mut self, error: impl Into<String>) {
        self.fatal_error = Some(error.into());
        self.running = false;
    }

    fn render(&mut self) -> Result<bool, WaylandError> {
        if !self.backend.active || self.backend.frame_pending {
            return Ok(false);
        }
        self.compositor.space.refresh();
        let geometry = self
            .compositor
            .space
            .output_geometry(&self.backend.output)
            .ok_or_else(|| WaylandError::Renderer("direct output is not mapped".to_owned()))?;
        let output_scale = self.backend.output.current_scale().fractional_scale();
        let output_offset = Point::<i32, Logical>::from((-geometry.loc.x, -geometry.loc.y))
            .to_physical_precise_round(output_scale);
        let mut renderer = self
            .backend
            .gpus
            .single_renderer(&self.backend.render_node)
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        let mut elements = Vec::new();
        if let Some((surface, location)) = self.compositor.cursor_surface_location() {
            let cursor_location = Point::<i32, Logical>::from((location.x, location.y));
            let local_location =
                (cursor_location - geometry.loc).to_physical_precise_round(output_scale);
            let cursor_elements: Vec<WaylandSurfaceRenderElement<_>> =
                render_elements_from_surface_tree(
                    &mut renderer,
                    &surface,
                    local_location,
                    output_scale,
                    1.0,
                    Kind::Cursor,
                );
            elements.extend(cursor_elements.into_iter().map(DirectRenderElement::from));
        }
        elements.extend(
            self.compositor
                .overlay_elements()
                .into_iter()
                .map(|element| {
                    DirectRenderElement::from(RelocateRenderElement::from_element(
                        element,
                        output_offset,
                        Relocate::Relative,
                    ))
                }),
        );
        let space_elements = self
            .compositor
            .space
            .render_elements_for_output(&mut renderer, &self.backend.output, 1.0)
            .map_err(|error| WaylandError::Renderer(error.to_string()))?;
        elements.extend(space_elements.into_iter().map(DirectRenderElement::from));
        elements.extend(
            self.compositor
                .decoration_elements()
                .into_iter()
                .map(|element| {
                    DirectRenderElement::from(RelocateRenderElement::from_element(
                        element,
                        output_offset,
                        Relocate::Relative,
                    ))
                }),
        );
        let result = self
            .backend
            .drm_output
            .render_frame(
                &mut renderer,
                &elements,
                Color32F::new(0.08, 0.10, 0.14, 1.0),
                FrameFlags::DEFAULT,
            )
            .map_err(|error| WaylandError::Renderer(format!("DRM render failed: {error}")))?;
        if result.is_empty {
            self.compositor.redraw_needed = false;
            return Ok(false);
        }
        if result.needs_sync()
            && let PrimaryPlaneElement::Swapchain(element) = &result.primary_element
        {
            element
                .sync
                .wait()
                .map_err(|error| WaylandError::Renderer(format!("render sync failed: {error}")))?;
        }
        drop(result);
        self.backend
            .drm_output
            .queue_frame(())
            .map_err(|error| WaylandError::Renderer(format!("KMS frame queue failed: {error}")))?;
        self.backend.frame_pending = true;
        self.compositor.redraw_needed = false;
        self.rendered_frames = self.rendered_frames.saturating_add(1);
        Ok(true)
    }
}

struct SelectedConnector {
    connector: smithay::reexports::drm::control::connector::Info,
    crtc: crtc::Handle,
    drm_mode: smithay::reexports::drm::control::Mode,
    state: super::DirectOutputState,
}

/// Runs the explicit direct-session backend with neutral reload and session handoff.
///
/// This function claims the active seat and DRM master. Call it only from the
/// explicit `--tty` path on a disposable or dedicated graphical session.
///
/// # Errors
///
/// Returns an error when libseat, DRM/GBM/GLES, connector selection, input,
/// Wayland dispatch, runtime control, or KMS presentation cannot start.
pub fn run_direct_with_session<G, E, R, RE, S>(
    options: DirectOptions,
    config: Config,
    restore: SessionRestore,
    control_ready: impl FnOnce(ControlSender) -> Result<G, E>,
    mut reload_config: R,
    mut save_session: S,
) -> Result<DirectRunReport, WaylandError>
where
    E: std::fmt::Display,
    R: FnMut() -> Result<Config, RE>,
    RE: std::fmt::Display,
    S: FnMut(&SessionSnapshot) -> bool,
{
    validate_socket_name(&options.socket_name)?;
    super::DirectDiagnostics::inspect().map_err(|error| {
        WaylandError::Initialization(format!("direct prerequisites failed: {error}"))
    })?;

    let mut event_loop = EventLoop::<DirectLoopData>::try_new()
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    let mut display = Display::<Compositor>::new()
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    let display_handle = display.handle();
    let disconnected = Arc::new(AtomicUsize::new(0));

    let (mut session, session_notifier) = LibSeatSession::new()
        .map_err(|error| WaylandError::Initialization(format!("libseat failed: {error}")))?;
    let udev = UdevBackend::new(session.seat())
        .map_err(|error| WaylandError::Initialization(format!("udev failed: {error}")))?;
    let (path, node) = udev
        .device_list()
        .filter_map(|(_, path)| {
            DrmNode::from_path(path)
                .ok()
                .map(|node| (path.to_path_buf(), node))
        })
        .next()
        .ok_or_else(|| WaylandError::Initialization("no seat DRM device found".to_owned()))?;
    let fd = session
        .open(
            &path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|error| {
            WaylandError::Initialization(format!("libseat DRM open failed: {error}"))
        })?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)
        .map_err(|error| WaylandError::Initialization(format!("DRM failed: {error}")))?;
    let gbm = GbmDevice::new(fd)
        .map_err(|error| WaylandError::Initialization(format!("GBM failed: {error}")))?;
    let render_node = node
        .node_with_type(NodeType::Render)
        .and_then(|node| node.ok())
        .unwrap_or(node);
    let mut gpus = GpuManager::new(DirectGbmBackend::with_context_priority(
        ContextPriority::High,
    ))
    .map_err(|error| WaylandError::Initialization(format!("GPU manager failed: {error}")))?;
    gpus.as_mut()
        .add_node(render_node, gbm.clone())
        .map_err(|error| WaylandError::Initialization(format!("GLES failed: {error}")))?;

    let mut scanner: DrmScanner = DrmScanner::new();
    let scan = scanner
        .scan_connectors(&drm)
        .map_err(|error| WaylandError::Initialization(format!("connector scan failed: {error}")))?;
    let connected = scan
        .into_iter()
        .filter_map(|event| match event {
            DrmScanEvent::Connected {
                connector,
                crtc: Some(crtc),
            } => Some((connector, crtc)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let inventory = connected
        .iter()
        .map(|(connector, _)| DirectConnector {
            name: connector_name(connector),
            modes: connector.modes().iter().copied().map(direct_mode).collect(),
        })
        .collect::<Vec<_>>();
    let topology = DirectTopology::plan(&config.outputs, inventory).map_err(|error| {
        WaylandError::Initialization(format!("output topology failed: {error}"))
    })?;
    let selected_state = topology
        .outputs
        .first()
        .cloned()
        .ok_or_else(|| WaylandError::Initialization("no selected output".to_owned()))?;
    if topology.outputs.len() != 1 {
        return Err(WaylandError::Initialization(format!(
            "the first direct KMS tranche requires exactly one enabled connector; planned {}",
            topology.outputs.len()
        )));
    }
    let (connector, crtc) = connected
        .into_iter()
        .find(|(connector, _)| connector_name(connector) == selected_state.name)
        .ok_or_else(|| WaylandError::Initialization("selected connector disappeared".to_owned()))?;
    let drm_mode = connector
        .modes()
        .iter()
        .copied()
        .find(|mode| direct_mode(*mode) == selected_state.mode)
        .ok_or_else(|| WaylandError::Initialization("selected mode disappeared".to_owned()))?;
    let selected = SelectedConnector {
        connector,
        crtc,
        drm_mode,
        state: selected_state,
    };

    let wl_mode = OutputMode::from(selected.drm_mode);
    let (physical_width, physical_height) = selected.connector.size().unwrap_or((0, 0));
    let output = Output::new(
        selected.state.name.clone(),
        PhysicalProperties {
            size: (
                i32::try_from(physical_width).unwrap_or(i32::MAX),
                i32::try_from(physical_height).unwrap_or(i32::MAX),
            )
                .into(),
            subpixel: Subpixel::Unknown,
            make: "Unknown".to_owned(),
            model: selected.state.name.clone(),
        },
    );
    let _global = output.create_global::<Compositor>(&display_handle);
    output.set_preferred(wl_mode);
    output.change_current_state(
        Some(wl_mode),
        Some(smithay_transform(selected.state.transform)),
        Some(Scale::Fractional(selected.state.scale.factor())),
        Some((0, 0).into()),
    );
    let logical_size = selected.state.logical_size();
    let compositor_size: smithay::utils::Size<i32, Physical> = (
        i32::try_from(logical_size.0).unwrap_or(i32::MAX),
        i32::try_from(logical_size.1).unwrap_or(i32::MAX),
    )
        .into();
    let compositor = Compositor::new(
        &display_handle,
        output.clone(),
        compositor_size,
        config,
        options.socket_name.clone().into(),
        restore,
    );

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(render_node));
    let mut renderer = gpus
        .single_renderer(&render_node)
        .map_err(|error| WaylandError::Initialization(format!("renderer failed: {error}")))?;
    let renderer_formats = renderer
        .as_mut()
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .copied()
        .collect::<Vec<_>>();
    let mut output_manager = DrmOutputManager::new(
        drm,
        allocator,
        exporter,
        Some(gbm),
        [Fourcc::Abgr8888, Fourcc::Argb8888],
        renderer_formats,
    );
    let initial_elements: DrmOutputRenderElements<_, SolidColorRenderElement> =
        DrmOutputRenderElements::default();
    let drm_output = output_manager
        .initialize_output(
            selected.crtc,
            selected.drm_mode,
            &[selected.connector.handle()],
            &output,
            None,
            &mut renderer,
            &initial_elements,
        )
        .map_err(|error| WaylandError::Initialization(format!("KMS output failed: {error}")))?;
    drop(renderer);

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&session.seat())
        .map_err(|error| {
            WaylandError::Initialization(format!("libinput seat failed: {error:?}"))
        })?;
    let input_backend = LibinputInputBackend::new(libinput.clone());

    let mut data = DirectLoopData {
        compositor,
        display_handle: display_handle.clone(),
        display_ready: false,
        rendered_frames: 0,
        fatal_error: None,
        running: true,
        reload_requested: false,
        session_save_requested: false,
        runtime_control: None,
        backend: DirectBackend {
            session,
            libinput,
            gpus,
            node,
            render_node,
            crtc: selected.crtc,
            output,
            output_manager,
            drm_output,
            active: true,
            frame_pending: false,
        },
    };

    let listener = ListeningSocketSource::with_name(&options.socket_name)
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    let socket_name = listener.socket_name().to_os_string();
    let client_disconnects = Arc::clone(&disconnected);
    event_loop
        .handle()
        .insert_source(listener, move |stream, _, data| {
            let client_data = Arc::new(WaylandClientState {
                compositor_state: Default::default(),
                disconnected: Arc::clone(&client_disconnects),
            });
            if let Err(error) = data.display_handle.insert_client(stream, client_data) {
                data.fail(format!("could not register Wayland client: {error}"));
            }
        })
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    let _control_guard = insert_runtime_control(&event_loop, &mut data, control_ready)?;
    let display_fd = display
        .as_fd()
        .try_clone_to_owned()
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(
            Generic::new(display_fd, Interest::READ, Mode::Level),
            |_, _, data| {
                data.display_ready = true;
                Ok(PostAction::Continue)
            },
        )
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(session_notifier, |event, _, data| match event {
            SessionEvent::PauseSession => {
                data.backend.libinput.suspend();
                data.backend.output_manager.pause();
                data.backend.active = false;
                data.backend.frame_pending = false;
                info!(seat = %data.backend.session.seat(), "direct Wayland session paused");
            }
            SessionEvent::ActivateSession => {
                if let Err(error) = data.backend.libinput.resume() {
                    data.fail(format!("libinput resume failed: {error:?}"));
                    return;
                }
                if let Err(error) = data.backend.output_manager.activate(true) {
                    data.fail(format!("DRM resume failed: {error}"));
                    return;
                }
                data.backend.active = true;
                data.compositor.redraw_needed = true;
                info!(seat = %data.backend.session.seat(), "direct Wayland session resumed");
            }
        })
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(drm_notifier, |event, _, data| match event {
            DrmEvent::VBlank(crtc) if crtc == data.backend.crtc => {
                if let Err(error) = data.backend.drm_output.frame_submitted() {
                    data.fail(format!("KMS frame completion failed: {error}"));
                    return;
                }
                data.backend.frame_pending = false;
                data.compositor.finish_frame_callbacks();
                data.compositor.redraw_needed = true;
            }
            DrmEvent::VBlank(_) => {}
            DrmEvent::Error(error) => data.fail(format!("DRM event failed: {error}")),
        })
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(input_backend, |event, _, data| {
            process_input_event(&mut data.compositor, event);
        })
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(udev, |event, _, data| {
            warn!(?event, node = %data.backend.node, "DRM topology changed; hotplug apply is the next W4 tranche");
        })
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;

    println!("ready: {}", socket_name.to_string_lossy());
    data.render()?;
    while data.running {
        event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut data)
            .map_err(|error| WaylandError::EventLoop(error.to_string()))?;
        if data.display_ready {
            data.display_ready = false;
            display
                .dispatch_clients(&mut data.compositor)
                .map_err(|error| WaylandError::EventLoop(error.to_string()))?;
        }
        if std::mem::take(&mut data.compositor.reload_requested) {
            data.reload_requested = true;
        }
        if data.compositor.exit_requested {
            data.running = false;
        }
        data.compositor.check_client_liveness();
        if data.compositor.redraw_needed && !data.backend.frame_pending {
            data.render()?;
        }
        display
            .flush_clients()
            .map_err(|error| WaylandError::EventLoop(error.to_string()))?;
        if std::mem::take(&mut data.reload_requested) {
            match reload_config() {
                Ok(config) => data.compositor.apply_config(config),
                Err(error) => {
                    warn!(%error, "direct Wayland reload rejected; retaining last good configuration")
                }
            }
        }
        if std::mem::take(&mut data.session_save_requested) {
            let snapshot = data.compositor.session_snapshot();
            if !save_session(&snapshot) {
                warn!("external direct Wayland session snapshot failed");
            }
        }
        if let Some(error) = data.fatal_error.take() {
            return Err(WaylandError::EventLoop(error));
        }
        if options.exit_after_disconnects > 0
            && disconnected.load(Ordering::Acquire) >= options.exit_after_disconnects
        {
            break;
        }
    }

    let snapshot = data.compositor.session_snapshot();
    Ok(DirectRunReport {
        socket_name,
        rendered_frames: data.rendered_frames,
        disconnected_clients: disconnected.load(Ordering::Acquire),
        snapshot,
        disposition: data.compositor.disposition.clone(),
    })
}

fn insert_runtime_control<G, E>(
    event_loop: &EventLoop<'static, DirectLoopData>,
    data: &mut DirectLoopData,
    control_ready: impl FnOnce(ControlSender) -> Result<G, E>,
) -> Result<G, WaylandError>
where
    E: std::fmt::Display,
{
    let (runtime_wake, runtime_events) = channel::channel();
    let runtime_control = ControlServer::bind(BackendKind::Wayland, move || {
        let _ = runtime_wake.send(());
    })?;
    let guard = control_ready(runtime_control.sender())
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    data.runtime_control = Some(runtime_control);
    event_loop
        .handle()
        .insert_source(runtime_events, |event, _, data| {
            if !matches!(event, ChannelEvent::Msg(())) {
                return;
            }
            let requests = data
                .runtime_control
                .as_ref()
                .map(|control| control.drain().collect::<Vec<_>>())
                .unwrap_or_default();
            for request in requests {
                match request {
                    ControlRequest::Reload => data.reload_requested = true,
                    ControlRequest::Shutdown => data.running = false,
                    ControlRequest::SaveSession => data.session_save_requested = true,
                }
            }
        })
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    Ok(guard)
}

fn process_input_event(compositor: &mut Compositor, event: InputEvent<LibinputInputBackend>) {
    match event {
        InputEvent::PointerMotion { event } => {
            compositor.pointer_motion_relative(event.delta().x, event.delta().y, event.time_msec())
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let size: smithay::utils::Size<i32, Logical> = (
                i32::try_from(compositor.output_geometry.width).unwrap_or(i32::MAX),
                i32::try_from(compositor.output_geometry.height).unwrap_or(i32::MAX),
            )
                .into();
            compositor.pointer_motion(
                event.x_transformed(size.w),
                event.y_transformed(size.h),
                event.time_msec(),
            );
        }
        InputEvent::PointerButton { event } => {
            compositor.pointer_button_code(event.button_code(), event.state(), event.time_msec())
        }
        InputEvent::PointerAxis { event } => {
            let mut frame =
                smithay::input::pointer::AxisFrame::new(event.time_msec()).source(event.source());
            for axis in [Axis::Horizontal, Axis::Vertical] {
                frame = frame.relative_direction(axis, event.relative_direction(axis));
                if let Some(amount) = event.amount(axis) {
                    frame = frame.value(axis, amount);
                }
                if let Some(amount) = event.amount_v120(axis) {
                    frame = frame.v120(axis, amount.round() as i32);
                }
            }
            compositor.pointer_axis(frame);
        }
        InputEvent::Keyboard { event } => {
            compositor.keyboard_keycode(event.key_code(), event.state(), event.time_msec());
        }
        _ => {}
    }
}

fn connector_name(connector: &smithay::reexports::drm::control::connector::Info) -> String {
    format!(
        "{}-{}",
        connector.interface().as_str(),
        connector.interface_id()
    )
}

fn direct_mode(mode: smithay::reexports::drm::control::Mode) -> DirectMode {
    let wl_mode = OutputMode::from(mode);
    DirectMode {
        width: u32::try_from(wl_mode.size.w).unwrap_or(0),
        height: u32::try_from(wl_mode.size.h).unwrap_or(0),
        refresh_millihz: u32::try_from(wl_mode.refresh).unwrap_or(0),
        preferred: mode.mode_type().contains(ModeTypeFlags::PREFERRED),
    }
}

const fn smithay_transform(transform: OutputTransform) -> Transform {
    match transform {
        OutputTransform::Normal => Transform::Normal,
        OutputTransform::Rotate90 => Transform::_90,
        OutputTransform::Rotate180 => Transform::_180,
        OutputTransform::Rotate270 => Transform::_270,
        OutputTransform::Flipped => Transform::Flipped,
        OutputTransform::Flipped90 => Transform::Flipped90,
        OutputTransform::Flipped180 => Transform::Flipped180,
        OutputTransform::Flipped270 => Transform::Flipped270,
    }
}
