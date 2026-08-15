//! Optional Smithay XWayland process and XWM boundary.

use std::{process::Stdio, sync::Arc, time::Duration};

use smithay::{
    reexports::{calloop::LoopHandle, wayland_server::DisplayHandle},
    wayland::xwayland_shell::XWaylandShellHandler,
    xwayland::{X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler, xwm::XwmId},
};
use tracing::{info, warn};

use super::{Compositor, WaylandClientState};

const RESTART_DELAY: Duration = Duration::from_secs(1);

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
        if let Some(token) = compositor.xwayland_source.take() {
            handle.remove(token);
        }
        compositor.xwm = None;
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
        self.xwm = None;
        self.xwayland_display = None;
        self.xwayland_restart_at = Some(std::time::Instant::now() + RESTART_DELAY);
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
                &mut <$state as crate::xwayland::LoopState>::compositor(self)
                    .xwayland_shell_state
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
                _window: smithay::xwayland::X11Surface,
            ) {
            }

            fn map_window_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
            ) {
                if let Err(error) = window.set_mapped(true) {
                    tracing::warn!(%error, window = window.window_id(), "could not map XWayland window");
                }
            }

            fn mapped_override_redirect_window(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                _window: smithay::xwayland::X11Surface,
            ) {
            }

            fn unmapped_window(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                _window: smithay::xwayland::X11Surface,
            ) {
            }

            fn destroyed_window(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                _window: smithay::xwayland::X11Surface,
            ) {
            }

            fn configure_request(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                window: smithay::xwayland::X11Surface,
                x: Option<i32>,
                y: Option<i32>,
                width: Option<u32>,
                height: Option<u32>,
                _reorder: Option<smithay::xwayland::xwm::Reorder>,
            ) {
                <$state as crate::xwayland::LoopState>::compositor(self)
                    .configure_x11_request(&window, x, y, width, height);
            }

            fn configure_notify(
                &mut self,
                _xwm: smithay::xwayland::xwm::XwmId,
                _window: smithay::xwayland::X11Surface,
                _geometry: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
                _above: Option<u32>,
            ) {
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
