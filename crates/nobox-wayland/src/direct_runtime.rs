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

use nobox_config::{Config, OutputTransform, OutputsConfig};
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
            AbsolutePositionEvent as _, Axis, Event as _, GestureBeginEvent as _,
            GestureEndEvent as _, GesturePinchUpdateEvent as _, GestureSwipeUpdateEvent as _,
            InputEvent, KeyboardKeyEvent as _, PointerAxisEvent as _, PointerButtonEvent as _,
            PointerMotionEvent as _, TouchEvent as _,
        },
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            Color32F, ImportAll, ImportDma, ImportMem,
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
        udev::{UdevBackend, UdevEvent},
    },
    desktop::space::SpaceRenderElements,
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, Mode, PostAction,
            channel::{self, Event as ChannelEvent},
            generic::Generic,
        },
        drm::control::{ModeTypeFlags, crtc},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::{Display, DisplayHandle, backend::GlobalId},
    },
    utils::{DeviceFd, Logical, Point, Transform},
    wayland::socket::ListeningSocketSource,
    wayland::{
        dmabuf::DmabufFeedbackBuilder,
        drm_syncobj::{DrmSyncobjState, supports_syncobj_eventfd},
    },
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};
use tracing::{debug, info, warn};

use super::{
    Compositor, CompositorOutput, DirectConnector, DirectMode, DirectOutputState, DirectTopology,
    WaylandClientState, WaylandError, validate_socket_name,
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
    scanner: DrmScanner,
    output_manager: DirectOutputManager,
    outputs: Vec<DirectSurface>,
    active: bool,
}

struct DirectSurface {
    connector: smithay::reexports::drm::control::connector::Info,
    crtc: crtc::Handle,
    output: Output,
    global: GlobalId,
    drm_output: DirectDrmOutput,
    state: DirectOutputState,
    frame_pending: bool,
}

struct DirectLoopData {
    loop_handle: LoopHandle<'static, DirectLoopData>,
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

    fn process_pending_dmabuf_imports(&mut self) {
        let mut pending = self.compositor.take_pending_dmabuf_imports();
        if pending.is_empty() {
            return;
        }
        let renderer = self.backend.gpus.single_renderer(&self.backend.render_node);
        let Ok(mut renderer) = renderer else {
            let error = renderer.unwrap_err();
            warn!(%error, count = pending.len(), "rejecting DMA-BUF imports while the renderer is unavailable");
            for import in pending {
                import.notifier.failed();
            }
            return;
        };
        while let Some(import) = pending.pop_front() {
            match renderer.import_dmabuf(&import.dmabuf, None) {
                Ok(_) => {
                    import.dmabuf.set_node(self.backend.render_node);
                    if import.notifier.successful::<Compositor>().is_err() {
                        debug!("DMA-BUF client disappeared before import completion");
                    }
                }
                Err(error) => {
                    warn!(%error, "rejected client DMA-BUF import; compositor remains usable");
                    import.notifier.failed();
                }
            }
        }
    }

    fn process_pending_surface_imports(&mut self) {
        for surface in self.compositor.take_pending_surface_imports() {
            if let Err(error) = self
                .backend
                .gpus
                .early_import(self.backend.render_node, &surface)
            {
                warn!(%error, "client buffer early import failed; omitting it until a later valid commit");
            }
        }
    }

    fn install_pending_syncobj_sources(&mut self) -> Result<(), WaylandError> {
        for pending in self.compositor.take_pending_syncobj_sources() {
            let client = pending.client;
            let display_handle = self.display_handle.clone();
            let active_sources = self.compositor.active_syncobj_sources.clone();
            if let Err(error) =
                self.loop_handle
                    .insert_source(pending.source, move |(), _, data| {
                        active_sources.fetch_sub(1, Ordering::Relaxed);
                        if let Some(client_state) = client.get_data::<WaylandClientState>() {
                            client_state
                                .compositor_state
                                .blocker_cleared(&mut data.compositor, &display_handle);
                        }
                        Ok(())
                    })
            {
                self.compositor
                    .active_syncobj_sources
                    .fetch_sub(1, Ordering::Relaxed);
                return Err(WaylandError::EventLoop(format!(
                    "could not watch an explicit-sync acquire fence: {error}"
                )));
            }
        }
        Ok(())
    }

    fn render_output(&mut self, output_index: usize) -> Result<bool, WaylandError> {
        if !self.backend.active || self.backend.outputs[output_index].frame_pending {
            return Ok(false);
        }
        let output = self.backend.outputs[output_index].output.clone();
        let geometry = self
            .compositor
            .space
            .output_geometry(&output)
            .ok_or_else(|| WaylandError::Renderer("direct output is not mapped".to_owned()))?;
        let output_scale = output.current_scale().fractional_scale();
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
        if let Some((surface, location)) = self.compositor.dnd_icon_surface_location() {
            let icon_location = Point::<i32, Logical>::from((location.x, location.y));
            let local_location =
                (icon_location - geometry.loc).to_physical_precise_round(output_scale);
            let icon_elements: Vec<WaylandSurfaceRenderElement<_>> =
                render_elements_from_surface_tree(
                    &mut renderer,
                    &surface,
                    local_location,
                    output_scale,
                    1.0,
                    Kind::Cursor,
                );
            elements.extend(icon_elements.into_iter().map(DirectRenderElement::from));
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
            .render_elements_for_output(&mut renderer, &output, 1.0)
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
        let result = self.backend.outputs[output_index]
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
        self.backend.outputs[output_index]
            .drm_output
            .queue_frame(())
            .map_err(|error| WaylandError::Renderer(format!("KMS frame queue failed: {error}")))?;
        self.backend.outputs[output_index].frame_pending = true;
        self.rendered_frames = self.rendered_frames.saturating_add(1);
        Ok(true)
    }

    fn render(&mut self) -> Result<bool, WaylandError> {
        if !self.backend.active {
            return Ok(false);
        }
        self.compositor.refresh_scene();
        let mut rendered = false;
        let mut waiting = false;
        for output_index in 0..self.backend.outputs.len() {
            if self.backend.outputs[output_index].frame_pending {
                waiting = true;
                continue;
            }
            rendered |= self.render_output(output_index)?;
        }
        if !waiting {
            self.compositor.redraw_needed = false;
        }
        Ok(rendered)
    }

    fn apply_existing_topology(&mut self, config: &Config) -> Result<(), WaylandError> {
        let inventory = self
            .backend
            .outputs
            .iter()
            .map(|output| DirectConnector {
                name: connector_name(&output.connector),
                modes: output
                    .connector
                    .modes()
                    .iter()
                    .copied()
                    .map(direct_mode)
                    .collect(),
            })
            .collect::<Vec<_>>();
        let topology = DirectTopology::plan(&config.outputs, inventory).map_err(|error| {
            WaylandError::Renderer(format!("output topology reload failed: {error}"))
        })?;
        if topology.outputs.len() != self.backend.outputs.len()
            || topology.outputs.iter().any(|state| {
                !self
                    .backend
                    .outputs
                    .iter()
                    .any(|output| output.output.name() == state.name)
            })
        {
            self.apply_scanned_topology(&config.outputs)?;
            return Ok(());
        }
        if topology.outputs.iter().all(|state| {
            self.backend
                .outputs
                .iter()
                .find(|output| output.output.name() == state.name)
                .is_some_and(|output| output.state == *state)
        }) {
            return Ok(());
        }

        let mut planned = Vec::with_capacity(topology.outputs.len());
        for state in topology.outputs {
            let output_index = self
                .backend
                .outputs
                .iter()
                .position(|output| output.output.name() == state.name)
                .ok_or_else(|| {
                    WaylandError::Renderer(
                        "live connector replacement awaits the hotplug transaction".to_owned(),
                    )
                })?;
            let surface = &self.backend.outputs[output_index];
            let drm_mode = surface
                .connector
                .modes()
                .iter()
                .copied()
                .find(|mode| direct_mode(*mode) == state.mode)
                .ok_or_else(|| {
                    WaylandError::Renderer(format!(
                        "configured mode for {} disappeared before KMS apply",
                        state.name
                    ))
                })?;
            planned.push((output_index, state, drm_mode));
        }

        let mode_changes = planned
            .iter()
            .filter(|(output_index, state, _)| {
                self.backend.outputs[*output_index].state.mode != state.mode
            })
            .map(|(output_index, _, drm_mode)| {
                let old_mode = self.backend.outputs[*output_index]
                    .connector
                    .modes()
                    .iter()
                    .copied()
                    .find(|mode| {
                        direct_mode(*mode) == self.backend.outputs[*output_index].state.mode
                    })
                    .expect("the active DRM mode remains in its connector inventory");
                (*output_index, *drm_mode, old_mode)
            })
            .collect::<Vec<_>>();
        if !mode_changes.is_empty() {
            let backend = &mut self.backend;
            let mut renderer = backend
                .gpus
                .single_renderer(&backend.render_node)
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            let elements: DrmOutputRenderElements<_, SolidColorRenderElement> =
                DrmOutputRenderElements::default();
            let mut applied: Vec<(usize, smithay::reexports::drm::control::Mode)> =
                Vec::with_capacity(mode_changes.len());
            for (output_index, new_mode, old_mode) in mode_changes {
                if let Err(error) = backend.outputs[output_index].drm_output.use_mode(
                    new_mode,
                    &mut renderer,
                    &elements,
                ) {
                    let mut rollback_failures = Vec::new();
                    for (applied_index, applied_old_mode) in applied.into_iter().rev() {
                        if let Err(rollback_error) = backend.outputs[applied_index]
                            .drm_output
                            .use_mode(applied_old_mode, &mut renderer, &elements)
                        {
                            rollback_failures.push(rollback_error.to_string());
                        }
                    }
                    let rollback = if rollback_failures.is_empty() {
                        "previous modes restored".to_owned()
                    } else {
                        format!("rollback failures: {}", rollback_failures.join("; "))
                    };
                    return Err(WaylandError::Renderer(format!(
                        "KMS mode candidate failed: {error}; {rollback}"
                    )));
                }
                applied.push((output_index, old_mode));
            }
        }

        let mut compositor_outputs = Vec::with_capacity(planned.len());
        for (output_index, state, drm_mode) in planned {
            let surface = &self.backend.outputs[output_index];
            let wl_mode = OutputMode::from(drm_mode);
            surface.output.change_current_state(
                Some(wl_mode),
                Some(smithay_transform(state.transform)),
                Some(Scale::Fractional(state.scale.factor())),
                Some((state.position.x, state.position.y).into()),
            );
            let logical_size = state.logical_size();
            compositor_outputs.push(CompositorOutput {
                output: surface.output.clone(),
                geometry: nobox_core::Geometry::new(
                    state.position.x,
                    state.position.y,
                    logical_size.0,
                    logical_size.1,
                ),
                primary: state.primary,
                global: Some(surface.global.clone()),
            });
            self.backend.outputs[output_index].state = state;
        }
        self.compositor.replace_outputs(compositor_outputs);
        Ok(())
    }

    fn sync_scene_from_surfaces(&mut self) {
        let outputs = self
            .backend
            .outputs
            .iter()
            .map(|surface| {
                let logical_size = surface.state.logical_size();
                CompositorOutput {
                    output: surface.output.clone(),
                    geometry: nobox_core::Geometry::new(
                        surface.state.position.x,
                        surface.state.position.y,
                        logical_size.0,
                        logical_size.1,
                    ),
                    primary: surface.state.primary,
                    global: Some(surface.global.clone()),
                }
            })
            .collect::<Vec<_>>();
        if !outputs.is_empty() {
            self.compositor.replace_outputs(outputs);
        }
    }

    fn rescan_outputs(&mut self) -> Result<bool, WaylandError> {
        let scan = self
            .backend
            .scanner
            .scan_connectors(self.backend.output_manager.device())
            .map_err(|error| WaylandError::Renderer(format!("connector rescan failed: {error}")))?;
        if scan.connected.is_empty() && scan.disconnected.is_empty() {
            return Ok(false);
        }
        let config = self.compositor.config.outputs.clone();
        self.apply_scanned_topology(&config)
    }

    fn apply_scanned_topology(&mut self, config: &OutputsConfig) -> Result<bool, WaylandError> {
        let connected = self
            .backend
            .scanner
            .crtcs()
            .map(|(connector, crtc)| (connector.clone(), crtc))
            .collect::<Vec<_>>();
        let inventory = connected
            .iter()
            .map(|(connector, _)| DirectConnector {
                name: connector_name(connector),
                modes: connector.modes().iter().copied().map(direct_mode).collect(),
            })
            .collect::<Vec<_>>();
        let previous_count = self.backend.outputs.len();
        self.backend.outputs.retain(|output| {
            connected.iter().any(|(connector, crtc)| {
                connector_name(connector) == output.output.name() && *crtc == output.crtc
            })
        });
        if self.backend.outputs.len() != previous_count && !self.backend.outputs.is_empty() {
            self.sync_scene_from_surfaces();
        }
        let topology = DirectTopology::plan(config, inventory)
            .map_err(|error| WaylandError::Renderer(format!("hotplug topology failed: {error}")))?;
        let mut available = connected;
        let mut selected = Vec::with_capacity(topology.outputs.len());
        for state in topology.outputs {
            let connector_index = available
                .iter()
                .position(|(connector, _)| connector_name(connector) == state.name)
                .ok_or_else(|| {
                    WaylandError::Renderer(format!(
                        "hotplug connector {} disappeared during apply",
                        state.name
                    ))
                })?;
            let (connector, crtc) = available.remove(connector_index);
            let drm_mode = connector
                .modes()
                .iter()
                .copied()
                .find(|mode| direct_mode(*mode) == state.mode)
                .ok_or_else(|| {
                    WaylandError::Renderer(format!(
                        "hotplug mode for {} disappeared during apply",
                        state.name
                    ))
                })?;
            selected.push(SelectedConnector {
                connector,
                crtc,
                drm_mode,
                state,
            });
        }

        for planned in &selected {
            let Some(existing) = self
                .backend
                .outputs
                .iter()
                .find(|output| output.output.name() == planned.state.name)
            else {
                continue;
            };
            if existing.crtc != planned.crtc
                || existing.state.mode != planned.state.mode
                || existing.state.transform != planned.state.transform
                || existing.state.scale != planned.state.scale
            {
                return Err(WaylandError::Renderer(format!(
                    "hotplug changed the active KMS assignment for {}; rollback support is required",
                    planned.state.name
                )));
            }
        }

        let current_names = self
            .backend
            .outputs
            .iter()
            .map(|output| output.output.name())
            .collect::<Vec<_>>();
        let planned_names = selected
            .iter()
            .map(|planned| planned.state.name.clone())
            .collect::<Vec<_>>();
        let delta = topology_delta(&current_names, &planned_names);
        let additions = selected
            .iter()
            .filter(|planned| delta.added.contains(&planned.state.name))
            .cloned()
            .collect::<Vec<_>>();

        let new_surfaces = (|| {
            let backend = &mut self.backend;
            let mut renderer = backend
                .gpus
                .single_renderer(&backend.render_node)
                .map_err(|error| WaylandError::Renderer(error.to_string()))?;
            let initial_elements: DrmOutputRenderElements<_, SolidColorRenderElement> =
                DrmOutputRenderElements::default();
            let mut surfaces = Vec::with_capacity(additions.len());
            for selected in additions {
                let output = direct_output(&selected);
                let drm_output = backend
                    .output_manager
                    .initialize_output(
                        selected.crtc,
                        selected.drm_mode,
                        &[selected.connector.handle()],
                        &output,
                        None,
                        &mut renderer,
                        &initial_elements,
                    )
                    .map_err(|error| {
                        WaylandError::Renderer(format!(
                            "hotplug KMS output {} failed: {error}",
                            selected.state.name
                        ))
                    })?;
                surfaces.push((selected, output, drm_output));
            }
            Ok::<_, WaylandError>(surfaces)
        })();
        let new_surfaces = match new_surfaces {
            Ok(surfaces) => surfaces,
            Err(error) => {
                self.sync_scene_from_surfaces();
                return Err(error);
            }
        };
        self.backend
            .outputs
            .retain(|output| !delta.removed.contains(&output.output.name()));
        for (selected, output, drm_output) in new_surfaces {
            let global = output.create_global::<Compositor>(&self.display_handle);
            self.backend.outputs.push(DirectSurface {
                connector: selected.connector,
                crtc: selected.crtc,
                output,
                global,
                drm_output,
                state: selected.state,
                frame_pending: false,
            });
        }

        for planned in &selected {
            let output = self
                .backend
                .outputs
                .iter_mut()
                .find(|output| output.output.name() == planned.state.name)
                .expect("planned KMS output was retained or initialized");
            output.connector = planned.connector.clone();
            output.state = planned.state.clone();
            output.output.change_current_state(
                None,
                None,
                None,
                Some((output.state.position.x, output.state.position.y).into()),
            );
        }
        let mut surfaces = std::mem::take(&mut self.backend.outputs);
        for planned in selected {
            let index = surfaces
                .iter()
                .position(|output| output.output.name() == planned.state.name)
                .expect("planned KMS output exists while ordering topology");
            self.backend.outputs.push(surfaces.remove(index));
        }
        self.sync_scene_from_surfaces();
        self.compositor.redraw_needed = true;
        Ok(true)
    }
}

#[derive(Clone)]
struct SelectedConnector {
    connector: smithay::reexports::drm::control::connector::Info,
    crtc: crtc::Handle,
    drm_mode: smithay::reexports::drm::control::Mode,
    state: super::DirectOutputState,
}

#[derive(Debug, Eq, PartialEq)]
struct TopologyDelta {
    removed: Vec<String>,
    added: Vec<String>,
}

fn topology_delta(current: &[String], planned: &[String]) -> TopologyDelta {
    TopologyDelta {
        removed: current
            .iter()
            .filter(|name| !planned.contains(name))
            .cloned()
            .collect(),
        added: planned
            .iter()
            .filter(|name| !current.contains(name))
            .cloned()
            .collect(),
    }
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
    let mut connected = connected;
    let mut selected = Vec::with_capacity(topology.outputs.len());
    for state in topology.outputs {
        let connector_index = connected
            .iter()
            .position(|(connector, _)| connector_name(connector) == state.name)
            .ok_or_else(|| {
                WaylandError::Initialization(format!(
                    "selected connector {} disappeared",
                    state.name
                ))
            })?;
        let (connector, crtc) = connected.remove(connector_index);
        let drm_mode = connector
            .modes()
            .iter()
            .copied()
            .find(|mode| direct_mode(*mode) == state.mode)
            .ok_or_else(|| {
                WaylandError::Initialization(format!(
                    "selected mode for {} disappeared",
                    state.name
                ))
            })?;
        selected.push(SelectedConnector {
            connector,
            crtc,
            drm_mode,
            state,
        });
    }

    let mut prepared_outputs = Vec::with_capacity(selected.len());
    let mut compositor_outputs = Vec::with_capacity(selected.len());
    for selected in selected {
        let output = direct_output(&selected);
        let global = output.create_global::<Compositor>(&display_handle);
        let logical_size = selected.state.logical_size();
        compositor_outputs.push(CompositorOutput {
            output: output.clone(),
            geometry: nobox_core::Geometry::new(
                selected.state.position.x,
                selected.state.position.y,
                logical_size.0,
                logical_size.1,
            ),
            primary: selected.state.primary,
            global: Some(global.clone()),
        });
        prepared_outputs.push((selected, output, global));
    }
    let mut compositor = Compositor::new_with_outputs(
        &display_handle,
        compositor_outputs,
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
    let mut direct_outputs = Vec::with_capacity(prepared_outputs.len());
    for (selected, output, global) in prepared_outputs {
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
            .map_err(|error| {
                WaylandError::Initialization(format!(
                    "KMS output {} failed: {error}",
                    selected.state.name
                ))
            })?;
        direct_outputs.push(DirectSurface {
            connector: selected.connector,
            crtc: selected.crtc,
            output,
            global,
            drm_output,
            state: selected.state,
            frame_pending: false,
        });
    }
    let default_feedback =
        DmabufFeedbackBuilder::new(render_node.dev_id(), renderer.dmabuf_formats())
            .build()
            .map_err(|error| {
                WaylandError::Initialization(format!("DMA-BUF feedback failed: {error}"))
            })?;
    let syncobj_device = output_manager.device().device_fd().clone();
    let syncobj_state = supports_syncobj_eventfd(&syncobj_device)
        .then(|| DrmSyncobjState::new::<Compositor>(&display_handle, syncobj_device));
    compositor.enable_direct_buffer_protocols(&default_feedback, syncobj_state);
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
        loop_handle: event_loop.handle(),
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
            scanner,
            output_manager,
            outputs: direct_outputs,
            active: true,
        },
    };

    let listener = ListeningSocketSource::with_name(&options.socket_name)
        .map_err(|error| WaylandError::Initialization(error.to_string()))?;
    let socket_name = listener.socket_name().to_os_string();
    let client_disconnects = Arc::clone(&disconnected);
    event_loop
        .handle()
        .insert_source(listener, move |stream, _, data| {
            let disconnected_client_ids = Arc::clone(&data.compositor.disconnected_client_ids);
            let client_data = Arc::new(WaylandClientState {
                compositor_state: Default::default(),
                disconnected: Arc::clone(&client_disconnects),
                surface_count: Arc::new(AtomicUsize::new(0)),
                selection_source_count: Arc::new(AtomicUsize::new(0)),
                selection_device_count: Arc::new(AtomicUsize::new(0)),
                pointer_extension_count: Arc::new(AtomicUsize::new(0)),
                pointer_gesture_count: Arc::new(AtomicUsize::new(0)),
                cursor_shape_count: Arc::new(AtomicUsize::new(0)),
                touch_device_count: Arc::new(AtomicUsize::new(0)),
                presentation_feedback_count: Arc::new(AtomicUsize::new(0)),
                shortcut_inhibitor_count: Arc::new(AtomicUsize::new(0)),
                disconnected_client_ids,
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
                for output in &mut data.backend.outputs {
                    output.frame_pending = false;
                }
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
            DrmEvent::VBlank(crtc) => {
                let Some(output_index) = data
                    .backend
                    .outputs
                    .iter()
                    .position(|output| output.crtc == crtc)
                else {
                    return;
                };
                if let Err(error) = data.backend.outputs[output_index]
                    .drm_output
                    .frame_submitted()
                {
                    data.fail(format!("KMS frame completion failed: {error}"));
                    return;
                }
                data.backend.outputs[output_index].frame_pending = false;
                let output = data.backend.outputs[output_index].output.clone();
                data.compositor.finish_frame_callbacks_for_output(&output);
                data.compositor.redraw_needed = true;
            }
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
            let event_kind = match event {
                UdevEvent::Added { .. } => "added",
                UdevEvent::Changed { .. } => "changed",
                UdevEvent::Removed { .. } => "removed",
            };
            match data.rescan_outputs() {
                Ok(true) => info!(event = event_kind, node = %data.backend.node, outputs = data.backend.outputs.len(), "applied DRM hotplug topology"),
                Ok(false) => {}
                Err(error) if data.backend.outputs.is_empty() => {
                    data.fail(format!("DRM hotplug left no usable output: {error}"));
                }
                Err(error) => {
                    warn!(%error, "DRM hotplug candidate rejected; retaining surviving outputs");
                    data.compositor.redraw_needed = true;
                }
            }
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
        data.compositor.cleanup_disconnected_selection_owners();
        data.process_pending_dmabuf_imports();
        data.install_pending_syncobj_sources()?;
        data.process_pending_surface_imports();
        if std::mem::take(&mut data.compositor.reload_requested) {
            data.reload_requested = true;
        }
        if data.compositor.exit_requested {
            data.running = false;
        }
        data.compositor.check_client_liveness();
        let outputs_idle = data
            .backend
            .outputs
            .iter()
            .all(|output| !output.frame_pending);
        if data.reload_requested && outputs_idle {
            data.reload_requested = false;
            match reload_config() {
                Ok(mut config) => {
                    if let Err(error) = data.apply_existing_topology(&config) {
                        warn!(%error, "direct output reload rejected; retaining live topology");
                        config.outputs = data.compositor.config.outputs.clone();
                    }
                    data.compositor.apply_config(config);
                }
                Err(error) => {
                    warn!(%error, "direct Wayland reload rejected; retaining last good configuration")
                }
            }
        }
        if data.compositor.redraw_needed && !data.reload_requested {
            data.render()?;
        }
        display
            .flush_clients()
            .map_err(|error| WaylandError::EventLoop(error.to_string()))?;
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
        InputEvent::PointerMotion { event } => compositor.pointer_motion_relative(
            event.delta().x,
            event.delta().y,
            event.delta_unaccel().x,
            event.delta_unaccel().y,
            event.time_msec(),
        ),
        InputEvent::PointerMotionAbsolute { event } => {
            let geometry = compositor.primary_output().geometry;
            let size: smithay::utils::Size<i32, Logical> = (
                i32::try_from(geometry.width).unwrap_or(i32::MAX),
                i32::try_from(geometry.height).unwrap_or(i32::MAX),
            )
                .into();
            compositor.pointer_motion(
                f64::from(geometry.x) + event.x_transformed(size.w),
                f64::from(geometry.y) + event.y_transformed(size.h),
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
        InputEvent::GestureSwipeBegin { event } => {
            compositor.pointer_gesture_swipe_begin(event.fingers(), event.time_msec());
        }
        InputEvent::GestureSwipeUpdate { event } => {
            compositor.pointer_gesture_swipe_update(event.delta(), event.time_msec());
        }
        InputEvent::GestureSwipeEnd { event } => {
            compositor.pointer_gesture_swipe_end(event.cancelled(), event.time_msec());
        }
        InputEvent::GesturePinchBegin { event } => {
            compositor.pointer_gesture_pinch_begin(event.fingers(), event.time_msec());
        }
        InputEvent::GesturePinchUpdate { event } => {
            compositor.pointer_gesture_pinch_update(
                event.delta(),
                event.scale(),
                event.rotation(),
                event.time_msec(),
            );
        }
        InputEvent::GesturePinchEnd { event } => {
            compositor.pointer_gesture_pinch_end(event.cancelled(), event.time_msec());
        }
        InputEvent::GestureHoldBegin { event } => {
            compositor.pointer_gesture_hold_begin(event.fingers(), event.time_msec());
        }
        InputEvent::GestureHoldEnd { event } => {
            compositor.pointer_gesture_hold_end(event.cancelled(), event.time_msec());
        }
        InputEvent::TouchDown { event } => {
            let geometry = compositor.primary_output().geometry;
            let size: smithay::utils::Size<i32, Logical> = (
                i32::try_from(geometry.width).unwrap_or(i32::MAX),
                i32::try_from(geometry.height).unwrap_or(i32::MAX),
            )
                .into();
            compositor.touch_down(
                (
                    f64::from(geometry.x) + event.x_transformed(size.w),
                    f64::from(geometry.y) + event.y_transformed(size.h),
                )
                    .into(),
                event.slot(),
                event.time_msec(),
            );
        }
        InputEvent::TouchMotion { event } => {
            let geometry = compositor.primary_output().geometry;
            let size: smithay::utils::Size<i32, Logical> = (
                i32::try_from(geometry.width).unwrap_or(i32::MAX),
                i32::try_from(geometry.height).unwrap_or(i32::MAX),
            )
                .into();
            compositor.touch_motion(
                (
                    f64::from(geometry.x) + event.x_transformed(size.w),
                    f64::from(geometry.y) + event.y_transformed(size.h),
                )
                    .into(),
                event.slot(),
                event.time_msec(),
            );
        }
        InputEvent::TouchUp { event } => {
            compositor.touch_up(event.slot(), event.time_msec());
        }
        InputEvent::TouchCancel { .. } => compositor.touch_cancel(),
        InputEvent::TouchFrame { .. } => compositor.touch_frame(),
        InputEvent::Keyboard { event } => {
            compositor.keyboard_keycode(event.key_code(), event.state(), event.time_msec());
        }
        _ => {}
    }
}

fn direct_output(selected: &SelectedConnector) -> Output {
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
    for mode in selected.connector.modes().iter().copied() {
        output.add_mode(OutputMode::from(mode));
    }
    let preferred_mode = selected
        .connector
        .modes()
        .iter()
        .copied()
        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
        .unwrap_or(selected.drm_mode);
    output.set_preferred(OutputMode::from(preferred_mode));
    output.change_current_state(
        Some(wl_mode),
        Some(smithay_transform(selected.state.transform)),
        Some(Scale::Fractional(selected.state.scale.factor())),
        Some((selected.state.position.x, selected.state.position.y).into()),
    );
    output
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topology_delta_preserves_removal_and_addition_order() {
        let current = vec!["DP-2".to_owned(), "eDP-1".to_owned()];
        let planned = vec!["eDP-1".to_owned(), "HDMI-A-1".to_owned()];
        assert_eq!(
            topology_delta(&current, &planned),
            TopologyDelta {
                removed: vec!["DP-2".to_owned()],
                added: vec!["HDMI-A-1".to_owned()],
            }
        );
        assert_eq!(
            topology_delta(&planned, &planned),
            TopologyDelta {
                removed: Vec::new(),
                added: Vec::new(),
            }
        );
    }
}
