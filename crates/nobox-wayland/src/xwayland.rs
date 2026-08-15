//! Optional Smithay XWayland process and XWM boundary.

use std::{os::fd::OwnedFd, process::Stdio, sync::Arc, time::Duration};

use nobox_config::{ApplicationIdentity, ApplicationKind, ApplicationWorkspace};
use nobox_core::{
    AspectRange, AspectRatio, Client as PolicyClient, ClientId as PolicyClientId, ClientLayer,
    ClientPolicy, ClientPresentation, ClientRole, DecorationOverride, Geometry, Gravity, Size,
    SizeHints, TransientTarget, WorkspaceAssignment, WorkspaceId,
};
use smithay::{
    desktop::Window,
    reexports::{
        calloop::{LoopHandle, channel, channel::Event as ChannelEvent},
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::DisplayHandle,
    },
    wayland::{
        selection::{
            SelectionTarget,
            data_device::{
                clear_data_device_selection, request_data_device_client_selection,
                set_data_device_selection,
            },
            primary_selection::{
                clear_primary_selection, request_primary_client_selection, set_primary_selection,
            },
        },
        xwayland_shell::XWaylandShellHandler,
    },
    xwayland::{
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
        xwm::{WmWindowProperty, WmWindowType, XwmId},
    },
};
use tracing::{info, warn};

use super::{
    Compositor, InteractiveKind, ManagedWindow, SelectionOrigin, SelectionUserData,
    WaylandClientState, application_layer, bounded_selection_mime_types, smart_placement,
};

const RESTART_DELAY: Duration = Duration::from_secs(1);
const MAX_UNMANAGED_X11_WINDOWS: usize = 128;

pub(crate) struct SelectionTransferRequest {
    pub(crate) xwm: XwmId,
    pub(crate) target: SelectionTarget,
    pub(crate) mime_type: String,
    pub(crate) fd: OwnedFd,
}

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

fn aspect_range(value: Option<((i32, i32), (i32, i32))>) -> Option<AspectRange> {
    let ((minimum_numerator, minimum_denominator), (maximum_numerator, maximum_denominator)) =
        value?;
    let minimum = AspectRatio::new(
        u32::try_from(minimum_numerator).ok()?,
        u32::try_from(minimum_denominator).ok()?,
    )?;
    let maximum = AspectRatio::new(
        u32::try_from(maximum_numerator).ok()?,
        u32::try_from(maximum_denominator).ok()?,
    )?;
    AspectRange::new(minimum, maximum)
}

fn x11_size_hints(window: &X11Surface) -> SizeHints {
    let hints = window.size_hints().unwrap_or_default();
    SizeHints {
        minimum: positive_size(hints.min_size),
        maximum: positive_size(hints.max_size),
        base: nonnegative_size(hints.base_size),
        increment: positive_size(hints.size_increment),
        aspect: aspect_range(hints.aspect.map(|(minimum, maximum)| {
            (
                (minimum.numerator, minimum.denominator),
                (maximum.numerator, maximum.denominator),
            )
        })),
    }
}

const fn pointer_button_code(button: u32) -> Option<u32> {
    match button {
        1 => Some(0x110),
        2 => Some(0x112),
        3 => Some(0x111),
        _ => None,
    }
}

pub(crate) const fn resize_edge(
    edge: smithay::xwayland::xwm::ResizeEdge,
) -> xdg_toplevel::ResizeEdge {
    match edge {
        smithay::xwayland::xwm::ResizeEdge::Top => xdg_toplevel::ResizeEdge::Top,
        smithay::xwayland::xwm::ResizeEdge::Bottom => xdg_toplevel::ResizeEdge::Bottom,
        smithay::xwayland::xwm::ResizeEdge::Left => xdg_toplevel::ResizeEdge::Left,
        smithay::xwayland::xwm::ResizeEdge::TopLeft => xdg_toplevel::ResizeEdge::TopLeft,
        smithay::xwayland::xwm::ResizeEdge::BottomLeft => xdg_toplevel::ResizeEdge::BottomLeft,
        smithay::xwayland::xwm::ResizeEdge::Right => xdg_toplevel::ResizeEdge::Right,
        smithay::xwayland::xwm::ResizeEdge::TopRight => xdg_toplevel::ResizeEdge::TopRight,
        smithay::xwayland::xwm::ResizeEdge::BottomRight => xdg_toplevel::ResizeEdge::BottomRight,
    }
}

#[cfg(test)]
mod tests {
    use super::{aspect_range, nonnegative_size, pointer_button_code, positive_size, resize_edge};
    use nobox_core::{AspectRange, AspectRatio, Size};
    use smithay::{
        reexports::wayland_protocols::xdg::shell::server::xdg_toplevel, xwayland::xwm::ResizeEdge,
    };

    #[test]
    fn x11_size_components_reject_invalid_values_but_allow_zero_base() {
        assert_eq!(positive_size(Some((20, 10))), Some(Size::new(20, 10)));
        assert_eq!(positive_size(Some((0, 10))), None);
        assert_eq!(positive_size(Some((-1, 10))), None);
        assert_eq!(
            nonnegative_size(Some((0, 0))),
            Some(Size {
                width: 0,
                height: 0
            })
        );
        assert_eq!(nonnegative_size(Some((-1, 0))), None);
    }

    #[test]
    fn x11_aspect_range_requires_positive_ordered_ratios() {
        let minimum = AspectRatio::new(3, 2).expect("valid minimum");
        let maximum = AspectRatio::new(2, 1).expect("valid maximum");
        assert_eq!(
            aspect_range(Some(((3, 2), (2, 1)))),
            AspectRange::new(minimum, maximum)
        );
        assert_eq!(aspect_range(Some(((0, 2), (2, 1)))), None);
        assert_eq!(aspect_range(Some(((2, 1), (3, 2)))), None);
    }

    #[test]
    fn x11_interactive_values_map_to_linux_input_and_wayland_edges() {
        assert_eq!(pointer_button_code(1), Some(0x110));
        assert_eq!(pointer_button_code(2), Some(0x112));
        assert_eq!(pointer_button_code(3), Some(0x111));
        assert_eq!(pointer_button_code(0), None);
        assert_eq!(pointer_button_code(4), None);
        assert_eq!(resize_edge(ResizeEdge::Top), xdg_toplevel::ResizeEdge::Top);
        assert_eq!(
            resize_edge(ResizeEdge::BottomRight),
            xdg_toplevel::ResizeEdge::BottomRight
        );
    }
}

pub(super) trait LoopState: XwmHandler + XWaylandShellHandler + Sized + 'static {
    fn compositor(&mut self) -> &mut Compositor;
}

pub(super) trait RuntimeLoopState: LoopState {
    fn loop_handle(&self) -> LoopHandle<'static, Self>;
}

pub(super) fn install_selection_bridge<D>(handle: &LoopHandle<'static, D>, data: &mut D)
where
    D: RuntimeLoopState,
{
    let (sender, receiver) = channel::channel::<SelectionTransferRequest>();
    match handle.insert_source(receiver, move |event, _, data| {
        let ChannelEvent::Msg(request) = event else {
            return;
        };
        let event_handle = data.loop_handle();
        let compositor = data.compositor();
        let Some(xwm) = compositor.xwm.as_mut() else {
            return;
        };
        if xwm.id() != request.xwm {
            return;
        }
        if let Err(error) = xwm.send_selection::<D>(
            request.target,
            request.mime_type,
            request.fd,
            event_handle.clone(),
        ) {
            warn!(%error, target = ?request.target, "could not request an XWayland selection transfer");
        }
    }) {
        Ok(_) => data.compositor().xwayland_selection_sender = Some(sender),
        Err(error) => warn!(%error, "could not install the XWayland selection bridge"),
    }
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
        compositor.clear_xwayland_selections();
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
                compositor.publish_wayland_selections_to_xwm();
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
        self.clear_xwayland_selections();
        self.xwm = None;
        self.xwayland_display = None;
        self.xwayland_restart_at = Some(std::time::Instant::now() + RESTART_DELAY);
    }

    pub(crate) fn selection_origin(&self, target: SelectionTarget) -> Option<SelectionOrigin> {
        match target {
            SelectionTarget::Clipboard => self.clipboard_selection_origin,
            SelectionTarget::Primary => self.primary_selection_origin,
        }
    }

    pub(crate) fn notify_xwayland_selection(
        &mut self,
        target: SelectionTarget,
        mime_types: Option<Vec<String>>,
    ) {
        let Some(xwm) = self.xwm.as_mut() else {
            return;
        };
        if let Err(error) = xwm.new_selection(target, mime_types) {
            warn!(%error, target = ?target, "could not publish a Wayland selection to XWayland");
        }
    }

    fn publish_wayland_selections_to_xwm(&mut self) {
        if self.clipboard_selection_origin == Some(SelectionOrigin::Wayland) {
            self.notify_xwayland_selection(
                SelectionTarget::Clipboard,
                Some(self.clipboard_mime_types.clone()),
            );
        }
        if self.primary_selection_origin == Some(SelectionOrigin::Wayland) {
            self.notify_xwayland_selection(
                SelectionTarget::Primary,
                Some(self.primary_selection_mime_types.clone()),
            );
        }
    }

    pub(crate) fn set_xwayland_selection(
        &mut self,
        xwm: XwmId,
        target: SelectionTarget,
        mime_types: Vec<String>,
    ) {
        if self.xwm.as_ref().is_none_or(|current| current.id() != xwm) {
            return;
        }
        let mime_types = bounded_selection_mime_types(mime_types);
        let user_data = SelectionUserData {
            origin: SelectionOrigin::XWayland(xwm),
        };
        match target {
            SelectionTarget::Clipboard => {
                self.clipboard_owner = None;
                self.clipboard_selection_origin = Some(user_data.origin);
                self.clipboard_mime_types = mime_types.clone();
                set_data_device_selection::<Compositor>(
                    &self.display_handle,
                    &self.seat,
                    mime_types,
                    user_data,
                );
            }
            SelectionTarget::Primary => {
                self.primary_selection_owner = None;
                self.primary_selection_origin = Some(user_data.origin);
                self.primary_selection_mime_types = mime_types.clone();
                set_primary_selection::<Compositor>(
                    &self.display_handle,
                    &self.seat,
                    mime_types,
                    user_data,
                );
            }
        }
        info!(target = ?target, "bridged an XWayland selection into the Wayland seat");
    }

    pub(crate) fn clear_xwayland_selection(&mut self, xwm: Option<XwmId>, target: SelectionTarget) {
        let origin = self.selection_origin(target);
        let owned = match (origin, xwm) {
            (Some(SelectionOrigin::XWayland(owner)), Some(expected)) => owner == expected,
            (Some(SelectionOrigin::XWayland(_)), None) => true,
            _ => false,
        };
        if !owned {
            return;
        }
        match target {
            SelectionTarget::Clipboard => {
                clear_data_device_selection(&self.display_handle, &self.seat);
                self.clipboard_selection_origin = None;
                self.clipboard_mime_types.clear();
            }
            SelectionTarget::Primary => {
                clear_primary_selection(&self.display_handle, &self.seat);
                self.primary_selection_origin = None;
                self.primary_selection_mime_types.clear();
            }
        }
    }

    fn clear_xwayland_selections(&mut self) {
        self.clear_xwayland_selection(None, SelectionTarget::Clipboard);
        self.clear_xwayland_selection(None, SelectionTarget::Primary);
    }

    pub(crate) fn allow_xwayland_selection_access(
        &self,
        xwm: XwmId,
        target: SelectionTarget,
    ) -> bool {
        self.xwm.as_ref().is_some_and(|current| current.id() == xwm)
            && self.selection_origin(target) == Some(SelectionOrigin::Wayland)
    }

    pub(crate) fn send_wayland_selection_to_xwayland(
        &self,
        xwm: XwmId,
        target: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        if !self.allow_xwayland_selection_access(xwm, target) {
            return;
        }
        let result = match target {
            SelectionTarget::Clipboard => {
                request_data_device_client_selection::<Compositor>(&self.seat, mime_type, fd)
                    .map_err(|error| error.to_string())
            }
            SelectionTarget::Primary => {
                request_primary_client_selection::<Compositor>(&self.seat, mime_type, fd)
                    .map_err(|error| error.to_string())
            }
        };
        if let Err(error) = result {
            warn!(%error, target = ?target, "could not send a Wayland selection to XWayland");
        }
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

    fn valid_x11_interactive_request(&self, window: &X11Surface, button: u32) -> bool {
        let Some(index) = self.x11_managed_index(window) else {
            return false;
        };
        let Some(button) = pointer_button_code(button) else {
            return false;
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        let Some(start) = pointer.grab_start_data() else {
            return false;
        };
        if start.button != button {
            return false;
        }
        let Some((focused, _)) = start.focus else {
            return false;
        };
        let mut belongs_to_window = false;
        self.windows[index].window.with_surfaces(|candidate, _| {
            belongs_to_window |= candidate == &focused;
        });
        belongs_to_window
    }

    pub(crate) fn start_x11_pointer_interactive(
        &mut self,
        window: &X11Surface,
        button: u32,
        kind: InteractiveKind,
    ) {
        if !self.valid_x11_interactive_request(window, button) {
            return;
        }
        let id = self.windows[self
            .x11_managed_index(window)
            .expect("validated XWayland window disappeared")]
        .id;
        self.start_pointer_interactive(Some(id), kind, self.pointer_location);
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
                window: smithay::xwayland::X11Surface,
                button: u32,
                edge: smithay::xwayland::xwm::ResizeEdge,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self)
                    .start_x11_pointer_interactive(
                        &window,
                        button,
                        crate::InteractiveKind::Resize(crate::xwayland::resize_edge(edge)),
                    );
            }

            fn move_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
                button: u32,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self)
                    .start_x11_pointer_interactive(&window, button, crate::InteractiveKind::Move);
            }

            fn allow_selection_access(
                &mut self,
                xwm: smithay::xwayland::xwm::XwmId,
                selection: smithay::wayland::selection::SelectionTarget,
            ) -> bool {
                <$state as crate::xwayland::LoopState>::compositor(self)
                    .allow_xwayland_selection_access(xwm, selection)
            }

            fn send_selection(
                &mut self,
                xwm: smithay::xwayland::xwm::XwmId,
                selection: smithay::wayland::selection::SelectionTarget,
                mime_type: String,
                fd: std::os::fd::OwnedFd,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self)
                    .send_wayland_selection_to_xwayland(xwm, selection, mime_type, fd);
            }

            fn new_selection(
                &mut self,
                xwm: smithay::xwayland::xwm::XwmId,
                selection: smithay::wayland::selection::SelectionTarget,
                mime_types: Vec<String>,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self)
                    .set_xwayland_selection(xwm, selection, mime_types);
            }

            fn cleared_selection(
                &mut self,
                xwm: smithay::xwayland::xwm::XwmId,
                selection: smithay::wayland::selection::SelectionTarget,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self)
                    .clear_xwayland_selection(Some(xwm), selection);
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
