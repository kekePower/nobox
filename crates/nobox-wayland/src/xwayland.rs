//! Optional Smithay XWayland process and XWM boundary.

use std::{process::Stdio, sync::Arc, time::Duration};

use nobox_config::{ApplicationIdentity, ApplicationKind, ApplicationWorkspace};
use nobox_core::{
    Client as PolicyClient, ClientId as PolicyClientId, ClientLayer, ClientPolicy,
    ClientPresentation, ClientRole, DecorationOverride, Geometry, Gravity, Size, SizeHints,
    TransientTarget, WorkspaceAssignment, WorkspaceId,
};
use smithay::{
    desktop::Window,
    reexports::{calloop::LoopHandle, wayland_server::DisplayHandle},
    wayland::xwayland_shell::XWaylandShellHandler,
    xwayland::{
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
        xwm::{WmWindowProperty, WmWindowType, XwmId},
    },
};
use tracing::{info, warn};

use super::{Compositor, ManagedWindow, WaylandClientState, application_layer, smart_placement};

const RESTART_DELAY: Duration = Duration::from_secs(1);
const MAX_UNMANAGED_X11_WINDOWS: usize = 128;

fn x11_role(window: &X11Surface) -> ClientRole {
    match window.window_type() {
        Some(WmWindowType::Dialog) => ClientRole::Dialog,
        Some(WmWindowType::Utility) => ClientRole::Utility,
        Some(WmWindowType::Toolbar) => ClientRole::Toolbar,
        Some(WmWindowType::Menu) => ClientRole::Menu,
        Some(WmWindowType::Splash) => ClientRole::Splash,
        Some(WmWindowType::DropdownMenu) => ClientRole::DropdownMenu,
        Some(WmWindowType::PopupMenu) => ClientRole::PopupMenu,
        Some(WmWindowType::Tooltip) => ClientRole::Tooltip,
        Some(WmWindowType::Notification) => ClientRole::Notification,
        Some(WmWindowType::Normal) | None => {
            if window.is_transient_for().is_some() {
                ClientRole::Dialog
            } else {
                ClientRole::Normal
            }
        }
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

fn positive_size(size: smithay::utils::Size<i32, smithay::utils::Logical>) -> Option<Size> {
    (size.w > 0 && size.h > 0).then(|| {
        Size::new(
            u32::try_from(size.w).unwrap_or(u32::MAX),
            u32::try_from(size.h).unwrap_or(u32::MAX),
        )
    })
}

fn x11_size_hints(window: &X11Surface) -> SizeHints {
    SizeHints {
        minimum: window.min_size().and_then(positive_size),
        maximum: window.max_size().and_then(positive_size),
        base: window.base_size().and_then(positive_size),
        ..SizeHints::default()
    }
}

pub(super) trait LoopState: XwmHandler + XWaylandShellHandler + Sized + 'static {
    fn compositor(&mut self) -> &mut Compositor;
}

pub(super) fn ensure_running<D>(
    handle: &LoopHandle<'static, D>,
    data: &mut D,
    display: &DisplayHandle,
) where
    D: LoopState,
{
    let compositor = data.compositor();
    if !compositor.config.wayland.xwayland {
        compositor.remove_all_x11_windows();
        if let Some(token) = compositor.xwayland_source.take() {
            handle.remove(token);
        }
        compositor.xwayland_display = None;
        compositor.xwayland_restart_at = None;
        return;
    }
    if compositor.xwm.is_some() {
        return;
    }
    match compositor.xwayland_restart_at {
        Some(deadline) if std::time::Instant::now() < deadline => return,
        None if compositor.xwayland_source.is_some() => return,
        _ => {}
    }
    if let Some(token) = compositor.xwayland_source.take() {
        handle.remove(token);
    }

    let disconnected = Arc::clone(&compositor.xwayland_disconnected);
    let disconnected_client_ids = Arc::clone(&compositor.disconnected_client_ids);
    let spawn = XWayland::spawn(
        display,
        None,
        std::iter::empty::<(&str, &str)>(),
        true,
        Stdio::null(),
        Stdio::null(),
        move |user_data| {
            user_data.insert_if_missing_threadsafe(|| {
                WaylandClientState::new(disconnected, disconnected_client_ids, false)
            });
        },
    );
    let (xwayland, client) = match spawn {
        Ok(instance) => instance,
        Err(error) => {
            warn!(%error, "could not start optional XWayland; native session remains available");
            data.compositor().xwayland_restart_at = Some(std::time::Instant::now() + RESTART_DELAY);
            return;
        }
    };
    let event_handle = handle.clone();
    let token = match handle.insert_source(xwayland, move |event, _, data| match event {
        XWaylandEvent::Ready {
            x11_socket,
            display_number,
        } => match X11Wm::start_wm(event_handle.clone(), x11_socket, client.clone()) {
            Ok(xwm) => {
                let compositor = data.compositor();
                compositor.xwayland_display = Some(format!(":{display_number}"));
                compositor.xwayland_restart_at = None;
                compositor.xwm = Some(xwm);
                info!(display = display_number, "XWayland and its XWM are ready");
            }
            Err(error) => {
                warn!(%error, "could not attach the XWayland XWM; native session remains available");
                data.compositor().schedule_xwayland_restart();
            }
        },
        XWaylandEvent::Error => {
            warn!("XWayland exited during startup; native session remains available");
            data.compositor().schedule_xwayland_restart();
        }
    }) {
        Ok(token) => token,
        Err(error) => {
            warn!(%error, "could not watch optional XWayland; native session remains available");
            data.compositor().xwayland_restart_at =
                Some(std::time::Instant::now() + RESTART_DELAY);
            return;
        }
    };
    let compositor = data.compositor();
    compositor.xwayland_source = Some(token);
    compositor.xwayland_restart_at = None;
}

impl Compositor {
    pub(crate) fn schedule_xwayland_restart(&mut self) {
        self.remove_all_x11_windows();
        self.xwm = None;
        self.xwayland_display = None;
        self.xwayland_restart_at = Some(std::time::Instant::now() + RESTART_DELAY);
    }

    pub(crate) fn x11_managed_index(&self, window: &X11Surface) -> Option<usize> {
        let window_id = window.window_id();
        self.windows.iter().position(|managed| {
            managed
                .window
                .x11_surface()
                .is_some_and(|candidate| candidate.window_id() == window_id)
        })
    }

    fn x11_parent(&self, window: &X11Surface) -> Option<PolicyClientId> {
        let parent = window.is_transient_for()?;
        self.windows.iter().find_map(|managed| {
            managed
                .window
                .x11_surface()
                .filter(|candidate| candidate.window_id() == parent)
                .map(|_| managed.id)
        })
    }

    pub(crate) fn manage_x11_window(&mut self, surface: X11Surface) {
        if self.x11_managed_index(&surface).is_some() {
            return;
        }
        let title = super::bounded_protocol_text(Some(&surface.title()), 1024);
        let class = super::bounded_protocol_text(Some(&surface.class()), 256);
        let instance = super::bounded_protocol_text(Some(&surface.instance()), 256);
        let role = x11_role(&surface);
        let parent = self.x11_parent(&surface);
        let modal = surface.is_popup() && parent.is_some();
        let application = self.config.application_settings(ApplicationIdentity {
            name: &instance,
            class: &class,
            group_name: "",
            group_class: "",
            role: "",
            title: &title,
            kind: application_kind(role),
        });
        let size_hints = x11_size_hints(&surface);
        let requested = surface.geometry();
        let requested_size = size_hints.constrain(Size::new(
            u32::try_from(requested.size.w.max(1)).unwrap_or(1),
            u32::try_from(requested.size.h.max(1)).unwrap_or(1),
        ));
        let natural_policy = if surface.is_decorated() {
            ClientPolicy::for_role(role).with_decoration_override(DecorationOverride::Undecorated)
        } else {
            ClientPolicy::for_role(role)
        };
        let decoration_override = match application.decorated {
            Some(true) => DecorationOverride::Decorated,
            Some(false) => DecorationOverride::Undecorated,
            None => DecorationOverride::Default,
        };
        let policy = natural_policy.with_decoration_override(decoration_override);
        let extents = policy.decorations.extents(
            self.config.theme.border_width,
            self.config.theme.titlebar_height,
        );
        let work_area = self.work_area();
        let outer_size = Size::new(
            requested_size
                .width
                .saturating_add(extents.left)
                .saturating_add(extents.right),
            requested_size
                .height
                .saturating_add(extents.top)
                .saturating_add(extents.bottom),
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
        let geometry = Geometry::new(
            placed
                .x
                .saturating_add(i32::try_from(extents.left).unwrap_or(i32::MAX)),
            placed
                .y
                .saturating_add(i32::try_from(extents.top).unwrap_or(i32::MAX)),
            requested_size.width,
            requested_size.height,
        );
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
        let layer = application
            .layer
            .map_or(ClientLayer::Normal, application_layer);
        let iconic = application.minimized.unwrap_or(surface.is_minimized());
        let id = PolicyClientId::new(self.next_client_id);
        self.next_client_id = self.next_client_id.saturating_add(1);
        let window = Window::new_x11_window(surface.clone());
        self.windows.push(ManagedWindow {
            id,
            window: window.clone(),
            title: title.clone(),
            app_id: class.clone(),
            foreign_toplevel: None,
            last_ping: std::time::Instant::now(),
            pending_ping: None,
        });
        if self.clients.showing_desktop()
            && !self.show_desktop_strict
            && role.occupies_placement_space()
        {
            self.clients.set_showing_desktop(false);
        }
        let _ = self.clients.manage(PolicyClient {
            id,
            geometry,
            size_hints,
            gravity: Gravity::default(),
            policy,
            natural_decorations: natural_policy.decorations,
            decoration_override,
            presentation: ClientPresentation {
                skip_taskbar: application.skip_taskbar.unwrap_or(false),
                skip_pager: application.skip_pager.unwrap_or(false),
                urgent: false,
            },
            transient_for: parent.map(TransientTarget::Client),
            group: None,
            modal,
            iconic,
            shaded: application.shaded.unwrap_or(false),
            workspace,
            layer,
            maximize: None,
            fullscreen: None,
            output_coverage: None,
        });
        let index = self.windows.len().saturating_sub(1);
        self.windows[index].foreign_toplevel = Some(
            self.foreign_toplevel_list_state
                .new_toplevel::<Self>(title, class.clone()),
        );
        self.add_wlr_foreign_toplevel(id);
        let focus_new = application.focus.unwrap_or(self.config.focus.focus_new);
        if !iconic && focus_new {
            let _ = self.clients.focus(id);
            if self.config.focus.raise_on_focus {
                let _ = self.clients.raise(id);
            }
        }
        let mut configured = geometry;
        if let Some((horizontal, vertical)) = application.maximized.map(|state| state.axes())
            && (horizontal || vertical)
            && let Some(maximized) =
                self.clients
                    .set_maximized(id, horizontal, vertical, self.work_area())
        {
            configured = maximized;
            let _ = surface.set_maximized(horizontal && vertical);
        }
        if application.fullscreen.unwrap_or(surface.is_fullscreen())
            && let Some(fullscreen) =
                self.clients
                    .set_fullscreen(id, true, self.primary_output().geometry)
        {
            configured = fullscreen;
            let _ = surface.set_fullscreen(true);
        }
        if let Err(error) = surface.configure(smithay::utils::Rectangle::new(
            (configured.x, configured.y).into(),
            (
                i32::try_from(configured.width).unwrap_or(i32::MAX),
                i32::try_from(configured.height).unwrap_or(i32::MAX),
            )
                .into(),
        )) {
            warn!(%error, "could not place managed XWayland window");
        }
        if let Err(error) = surface.set_mapped(true) {
            warn!(%error, "could not map managed XWayland window");
        }
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
        info!(
            client = id.raw(),
            class, "managed XWayland window through core policy"
        );
    }

    pub(crate) fn update_x11_window(&mut self, surface: &X11Surface, property: WmWindowProperty) {
        let Some(index) = self.x11_managed_index(surface) else {
            return;
        };
        let id = self.windows[index].id;
        match property {
            WmWindowProperty::Title | WmWindowProperty::Class => {
                self.windows[index].title =
                    super::bounded_protocol_text(Some(&surface.title()), 1024);
                self.windows[index].app_id =
                    super::bounded_protocol_text(Some(&surface.class()), 256);
                if let Some(handle) = &self.windows[index].foreign_toplevel {
                    handle.send_title(&self.windows[index].title);
                    handle.send_app_id(&self.windows[index].app_id);
                    handle.send_done();
                }
            }
            WmWindowProperty::NormalHints => {
                let _ = self.clients.set_size_hints(id, x11_size_hints(surface));
            }
            WmWindowProperty::TransientFor | WmWindowProperty::WindowType => {
                let parent = self.x11_parent(surface);
                let _ = self.clients.set_relationships(
                    id,
                    parent.map(TransientTarget::Client),
                    None,
                    surface.is_popup() && parent.is_some(),
                );
            }
            WmWindowProperty::Protocols
            | WmWindowProperty::Hints
            | WmWindowProperty::MotifHints
            | WmWindowProperty::StartupId
            | WmWindowProperty::Pid => {}
        }
        self.sync_wlr_foreign_toplevel_protocol();
        self.redraw_needed = true;
    }

    pub(crate) fn configure_managed_x11(
        &mut self,
        surface: &X11Surface,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        let Some(index) = self.x11_managed_index(surface) else {
            self.configure_x11_request(surface, None, None, width, height);
            return;
        };
        let id = self.windows[index].id;
        let Some(current) = self.clients.get(id).copied() else {
            return;
        };
        let requested = current.size_hints.constrain(Size::new(
            width.unwrap_or(current.geometry.width),
            height.unwrap_or(current.geometry.height),
        ));
        let geometry = Geometry::new(
            current.geometry.x,
            current.geometry.y,
            requested.width,
            requested.height,
        );
        let _ = self.clients.set_geometry(id, geometry);
        self.configure_x11_request(
            surface,
            Some(geometry.x),
            Some(geometry.y),
            Some(geometry.width),
            Some(geometry.height),
        );
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
    }

    pub(crate) fn remove_x11_window(&mut self, surface: &X11Surface) {
        let window_id = surface.window_id();
        if let Some(index) = self.x11_managed_index(surface) {
            let mut managed = self.windows.remove(index);
            self.space.unmap_elem(&managed.window);
            if let Some(handle) = managed.foreign_toplevel.take() {
                self.foreign_toplevel_list_state.remove_toplevel(&handle);
            }
            self.remove_wlr_foreign_toplevel(managed.id);
            let _ = self.clients.unmanage(managed.id);
            self.session_stacking.remove(&managed.id);
            self.remove_focus_cycle_candidate(managed.id);
        }
        if let Some(index) = self.x11_unmanaged.iter().position(|window| {
            window
                .x11_surface()
                .is_some_and(|candidate| candidate.window_id() == window_id)
        }) {
            let window = self.x11_unmanaged.remove(index);
            self.space.unmap_elem(&window);
        }
        self.sync_focus_and_stacking();
        self.redraw_needed = true;
    }

    fn remove_all_x11_windows(&mut self) {
        let managed = self
            .windows
            .iter()
            .filter_map(|managed| managed.window.x11_surface().cloned())
            .collect::<Vec<_>>();
        for window in managed {
            self.remove_x11_window(&window);
        }
        for window in std::mem::take(&mut self.x11_unmanaged) {
            self.space.unmap_elem(&window);
        }
        self.redraw_needed = true;
    }

    pub(crate) fn track_unmanaged_x11(&mut self, surface: X11Surface) {
        if self.x11_unmanaged.len() >= MAX_UNMANAGED_X11_WINDOWS {
            warn!(
                limit = MAX_UNMANAGED_X11_WINDOWS,
                "ignored excess unmanaged XWayland window"
            );
            return;
        }
        if self.x11_unmanaged.iter().any(|window| {
            window
                .x11_surface()
                .is_some_and(|candidate| candidate.window_id() == surface.window_id())
        }) {
            return;
        }
        self.x11_unmanaged.push(Window::new_x11_window(surface));
    }

    pub(crate) fn map_unmanaged_x11(&mut self, surface: &X11Surface) {
        self.track_unmanaged_x11(surface.clone());
        let Some(window) = self.x11_unmanaged.iter().find(|window| {
            window
                .x11_surface()
                .is_some_and(|candidate| candidate.window_id() == surface.window_id())
        }) else {
            return;
        };
        let geometry = surface.geometry();
        self.space.map_element(window.clone(), geometry.loc, true);
        self.redraw_needed = true;
        if surface.wl_surface().is_some() {
            info!(
                x = geometry.loc.x,
                y = geometry.loc.y,
                "mapped unmanaged XWayland surface"
            );
        }
    }

    pub(crate) fn commit_x11_surface(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        let managed = self.windows.iter().any(|managed| {
            managed
                .window
                .x11_surface()
                .and_then(X11Surface::wl_surface)
                .as_ref()
                == Some(surface)
        });
        if managed {
            self.sync_focus_and_stacking();
            self.redraw_needed = true;
        }
        let unmanaged = self
            .x11_unmanaged
            .iter()
            .filter_map(|window| window.x11_surface())
            .find(|candidate| candidate.wl_surface().as_ref() == Some(surface))
            .cloned();
        if let Some(unmanaged) = unmanaged {
            self.map_unmanaged_x11(&unmanaged);
        }
    }

    pub(crate) fn xwm(&mut self, expected: XwmId) -> &mut X11Wm {
        let xwm = self.xwm.as_mut().expect("XWM callback without a live XWM");
        assert_eq!(xwm.id(), expected, "XWM callback used a stale generation");
        xwm
    }

    pub(crate) fn configure_x11_request(
        &mut self,
        window: &X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        let mut geometry = window.geometry();
        if let Some(x) = x {
            geometry.loc.x = x;
        }
        if let Some(y) = y {
            geometry.loc.y = y;
        }
        if let Some(width) = width {
            geometry.size.w = i32::try_from(width).unwrap_or(i32::MAX).max(1);
        }
        if let Some(height) = height {
            geometry.size.h = i32::try_from(height).unwrap_or(i32::MAX).max(1);
        }
        if let Err(error) = window.configure(geometry) {
            warn!(%error, window = window.window_id(), "could not configure XWayland window");
        }
    }
}

impl LoopState for Compositor {
    fn compositor(&mut self) -> &mut Compositor {
        self
    }
}

macro_rules! impl_loop_handlers {
    ($state:ty) => {
        impl smithay::wayland::xwayland_shell::XWaylandShellHandler for $state {
            fn xwayland_shell_state(
                &mut self,
            ) -> &mut smithay::wayland::xwayland_shell::XWaylandShellState {
                &mut <$state as crate::xwayland::LoopState>::compositor(self).xwayland_shell_state
            }

            fn surface_associated(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                _wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
                window: smithay::xwayland::X11Surface,
            ) {
                let compositor = <$state as crate::xwayland::LoopState>::compositor(self);
                if window.is_override_redirect() {
                    compositor.map_unmanaged_x11(&window);
                } else {
                    compositor.sync_focus_and_stacking();
                    compositor.redraw_needed = true;
                }
            }
        }

        impl smithay::xwayland::XwmHandler for $state {
            fn xwm_state(
                &mut self,
                xwm: smithay::xwayland::xwm::XwmId,
            ) -> &mut smithay::xwayland::X11Wm {
                <$state as crate::xwayland::LoopState>::compositor(self).xwm(xwm)
            }

            fn new_window(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                _window: smithay::xwayland::X11Surface,
            ) {
            }

            fn new_override_redirect_window(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self)
                    .track_unmanaged_x11(window);
            }

            fn map_window_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self).manage_x11_window(window);
            }

            fn map_window_notify(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                _window: smithay::xwayland::X11Surface,
            ) {
            }

            fn mapped_override_redirect_window(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self).map_unmanaged_x11(&window);
            }

            fn unmapped_window(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self).remove_x11_window(&window);
            }

            fn destroyed_window(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self).remove_x11_window(&window);
            }

            fn configure_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
                _x: Option<i32>,
                _y: Option<i32>,
                width: Option<u32>,
                height: Option<u32>,
                _reorder: Option<smithay::xwayland::xwm::Reorder>,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self)
                    .configure_managed_x11(&window, width, height);
            }

            fn configure_notify(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
                geometry: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
                _above: Option<u32>,
            ) {
                let compositor = <$state as crate::xwayland::LoopState>::compositor(self);
                if window.is_override_redirect() {
                    compositor.map_unmanaged_x11(&window);
                } else if let Some(index) = compositor.x11_managed_index(&window) {
                    let id = compositor.windows[index].id;
                    let _ = compositor.clients.set_geometry(
                        id,
                        nobox_core::Geometry::new(
                            geometry.loc.x,
                            geometry.loc.y,
                            u32::try_from(geometry.size.w.max(1)).unwrap_or(1),
                            u32::try_from(geometry.size.h.max(1)).unwrap_or(1),
                        ),
                    );
                    compositor.sync_focus_and_stacking();
                    compositor.redraw_needed = true;
                }
            }

            fn property_notify(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
                property: smithay::xwayland::xwm::WmWindowProperty,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self)
                    .update_x11_window(&window, property);
            }

            fn maximize_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                let compositor = <$state as crate::xwayland::LoopState>::compositor(self);
                if let Some(index) = compositor.x11_managed_index(&window) {
                    let id = compositor.windows[index].id;
                    if let Some(geometry) =
                        compositor
                            .clients
                            .set_maximized(id, true, true, compositor.work_area())
                    {
                        let _ = window.set_maximized(true);
                        compositor.configure_x11_request(
                            &window,
                            Some(geometry.x),
                            Some(geometry.y),
                            Some(geometry.width),
                            Some(geometry.height),
                        );
                        compositor.sync_focus_and_stacking();
                    }
                }
            }

            fn unmaximize_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                let compositor = <$state as crate::xwayland::LoopState>::compositor(self);
                if let Some(index) = compositor.x11_managed_index(&window) {
                    let id = compositor.windows[index].id;
                    if let Some(geometry) =
                        compositor
                            .clients
                            .set_maximized(id, false, false, compositor.work_area())
                    {
                        let _ = window.set_maximized(false);
                        compositor.configure_x11_request(
                            &window,
                            Some(geometry.x),
                            Some(geometry.y),
                            Some(geometry.width),
                            Some(geometry.height),
                        );
                        compositor.sync_focus_and_stacking();
                    }
                }
            }

            fn fullscreen_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                let compositor = <$state as crate::xwayland::LoopState>::compositor(self);
                if let Some(index) = compositor.x11_managed_index(&window) {
                    let id = compositor.windows[index].id;
                    if let Some(geometry) = compositor.clients.set_fullscreen(
                        id,
                        true,
                        compositor.primary_output().geometry,
                    ) {
                        let _ = window.set_fullscreen(true);
                        compositor.configure_x11_request(
                            &window,
                            Some(geometry.x),
                            Some(geometry.y),
                            Some(geometry.width),
                            Some(geometry.height),
                        );
                        compositor.sync_focus_and_stacking();
                    }
                }
            }

            fn unfullscreen_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                let compositor = <$state as crate::xwayland::LoopState>::compositor(self);
                if let Some(index) = compositor.x11_managed_index(&window) {
                    let id = compositor.windows[index].id;
                    if let Some(geometry) = compositor.clients.set_fullscreen(
                        id,
                        false,
                        compositor.primary_output().geometry,
                    ) {
                        let _ = window.set_fullscreen(false);
                        compositor.configure_x11_request(
                            &window,
                            Some(geometry.x),
                            Some(geometry.y),
                            Some(geometry.width),
                            Some(geometry.height),
                        );
                        compositor.sync_focus_and_stacking();
                    }
                }
            }

            fn minimize_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                let compositor = <$state as crate::xwayland::LoopState>::compositor(self);
                if let Some(index) = compositor.x11_managed_index(&window) {
                    let id = compositor.windows[index].id;
                    let _ = compositor.clients.set_iconic(id, true);
                    let _ = window.set_suspended(true);
                    compositor.sync_focus_and_stacking();
                    compositor.redraw_needed = true;
                }
            }

            fn unminimize_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                let compositor = <$state as crate::xwayland::LoopState>::compositor(self);
                if let Some(index) = compositor.x11_managed_index(&window) {
                    let id = compositor.windows[index].id;
                    let _ = compositor.clients.set_iconic(id, false);
                    let _ = window.set_suspended(false);
                    compositor.sync_focus_and_stacking();
                    compositor.redraw_needed = true;
                }
            }

            fn resize_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                _window: smithay::xwayland::X11Surface,
                _button: u32,
                _resize_edge: smithay::xwayland::xwm::ResizeEdge,
            ) {
            }

            fn move_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                _window: smithay::xwayland::X11Surface,
                _button: u32,
            ) {
            }

            fn disconnected(&mut self, _xwm: smithay::xwayland::xwm::XwmId) {
                tracing::warn!("XWayland connection closed; scheduling an isolated restart");
                <$state as crate::xwayland::LoopState>::compositor(self)
                    .schedule_xwayland_restart();
            }
        }
    };
}

pub(super) use impl_loop_handlers;

impl_loop_handlers!(Compositor);
