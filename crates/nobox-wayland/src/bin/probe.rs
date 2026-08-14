//! Deterministically probe a Nobox Wayland compositor.

use std::{
    fs::{File, OpenOptions},
    io::{Seek as _, SeekFrom, Write as _},
    os::fd::AsFd as _,
    path::PathBuf,
};

use anyhow::{Context as _, Result, ensure};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat,
        wl_shm, wl_shm_pool, wl_subcompositor, wl_subsurface, wl_surface,
    },
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
};
use x11rb::{
    CURRENT_TIME,
    connection::Connection as _,
    protocol::{
        xproto::{
            BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, InputFocus,
            KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
        },
        xtest::ConnectionExt as _,
    },
};

struct Probe;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Probe {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
    }
}

fn main() -> Result<()> {
    let mode = std::env::args().nth(1);
    match mode.as_deref() {
        Some("--shell") => return probe_shell(false),
        Some("--shell-input") => return probe_shell(true),
        Some("--invalid-configure") => return probe_protocol_error(ProtocolViolation::Configure),
        Some("--invalid-role") => return probe_protocol_error(ProtocolViolation::Role),
        Some("--unresponsive") => return probe_unresponsive(),
        Some("--close") => return probe_close(),
        Some("--popup-grab") => return probe_popup_grab(),
        _ => {}
    }
    let connection = Connection::connect_to_env()?;
    let (globals, _event_queue) = registry_queue_init::<Probe>(&connection)?;
    let mut interfaces = globals.contents().clone_list();
    interfaces.sort_by(|left, right| left.interface.cmp(&right.interface));
    for global in interfaces {
        println!("{} {}", global.interface, global.version);
    }
    Ok(())
}

fn probe_shell(inject_input: bool) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        ..ShellProbe::default()
    };
    event_queue.roundtrip(&mut state)?;
    ensure!(
        state.toplevel.is_some(),
        "xdg-shell did not create a toplevel"
    );
    ensure!(
        state.buffer.is_some(),
        "wl_shm did not create a test buffer"
    );

    event_queue.roundtrip(&mut state)?;
    ensure!(
        state.configured,
        "toplevel never received its initial configure"
    );
    event_queue.roundtrip(&mut state)?;
    ensure!(
        state.frame_callbacks > 0,
        "mapped surface received no frame callback"
    );
    if inject_input {
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 70, 70)])?;
        for _ in 0..2 {
            event_queue.roundtrip(&mut state)?;
        }
        inject_parent_input(&[(BUTTON_PRESS_EVENT, 1, 70, 70)])?;
        for _ in 0..3 {
            event_queue.roundtrip(&mut state)?;
        }
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 90, 85)])?;
        event_queue.roundtrip(&mut state)?;
        inject_parent_input(&[(BUTTON_RELEASE_EVENT, 1, 90, 85)])?;
        event_queue.roundtrip(&mut state)?;
        inject_parent_input(&[
            (MOTION_NOTIFY_EVENT, 0, 110, 95),
            (BUTTON_PRESS_EVENT, 1, 110, 95),
        ])?;
        for _ in 0..3 {
            event_queue.roundtrip(&mut state)?;
        }
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 175, 140)])?;
        event_queue.roundtrip(&mut state)?;
        inject_parent_input(&[(BUTTON_RELEASE_EVENT, 1, 175, 140)])?;
        event_queue.roundtrip(&mut state)?;
        inject_parent_input(&[
            (MOTION_NOTIFY_EVENT, 0, 130, 110),
            (BUTTON_PRESS_EVENT, 1, 130, 110),
        ])?;
        for _ in 0..3 {
            event_queue.roundtrip(&mut state)?;
        }
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 260, 210)])?;
        event_queue.roundtrip(&mut state)?;
        inject_parent_input(&[(BUTTON_RELEASE_EVENT, 1, 260, 210)])?;
        event_queue.roundtrip(&mut state)?;
        inject_parent_input(&[(KEY_PRESS_EVENT, 38, 0, 0), (KEY_RELEASE_EVENT, 38, 0, 0)])?;
        event_queue.roundtrip(&mut state)?;
        ensure!(
            state.interaction_count >= 2,
            "pointer produced only {} of 2 move/resize requests",
            state.interaction_count
        );
        ensure!(
            state.last_configure_size.is_some_and(|(width, height)| {
                (120..=180).contains(&width) && (80..=120).contains(&height)
            }),
            "resize did not honor xdg min/max constraints: {:?}",
            state.last_configure_size
        );
        ensure!(
            state.key_events >= 2,
            "keyboard focus did not receive the injected key stroke"
        );
    }

    let toplevel = state.toplevel.clone().expect("checked above");
    toplevel.set_maximized();
    event_queue.roundtrip(&mut state)?;
    toplevel.unset_maximized();
    event_queue.roundtrip(&mut state)?;
    toplevel.set_fullscreen(None);
    event_queue.roundtrip(&mut state)?;
    toplevel.unset_fullscreen();
    event_queue.roundtrip(&mut state)?;
    ensure!(
        state.configure_count >= 5,
        "state requests did not produce configures"
    );
    toplevel.set_minimized();
    event_queue.roundtrip(&mut state)?;

    if let Some(surface) = &state.surface {
        surface.attach(None, 0, 0);
        surface.commit();
    }
    event_queue.roundtrip(&mut state)?;
    println!(
        "shell-ok configures={} frames={}",
        state.configure_count, state.frame_callbacks
    );
    Ok(())
}

fn probe_protocol_error(violation: ProtocolViolation) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        violation: Some(violation),
        respond_to_ping: true,
        ..ShellProbe::default()
    };
    for _ in 0..4 {
        if event_queue.roundtrip(&mut state).is_err() {
            println!("protocol-error-ok");
            return Ok(());
        }
    }
    anyhow::bail!("invalid client was not disconnected")
}

fn probe_unresponsive() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe::default();
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.configured {
            break;
        }
    }
    ensure!(state.configured, "unresponsive client did not map");
    std::thread::sleep(std::time::Duration::from_secs(4));
    let read_failed = connection
        .prepare_read()
        .is_none_or(|guard| guard.read().is_err());
    let _ = event_queue.dispatch_pending(&mut state);
    let disconnected = read_failed || connection.protocol_error().is_some();
    ensure!(
        disconnected,
        "compositor did not reject the unresponsive client"
    );
    println!("unresponsive-ok");
    Ok(())
}

fn probe_close() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        ..ShellProbe::default()
    };
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.configured && state.frame_callbacks > 0 {
            break;
        }
    }
    ensure!(state.configured, "close probe did not map");
    inject_parent_input(&[(KEY_PRESS_EVENT, 9, 0, 0), (KEY_RELEASE_EVENT, 9, 0, 0)])?;
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.close_received {
            println!("close-ok");
            return Ok(());
        }
    }
    anyhow::bail!("focused client did not receive xdg_toplevel.close")
}

fn probe_popup_grab() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        request_popup_grab: true,
        ..ShellProbe::default()
    };
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.configured && state.frame_callbacks > 0 {
            break;
        }
    }
    ensure!(state.configured, "popup-grab probe did not map");
    inject_parent_input(&[
        (MOTION_NOTIFY_EVENT, 0, 70, 70),
        (BUTTON_PRESS_EVENT, 1, 70, 70),
    ])?;
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.popup_grab_requested {
            println!("popup-grab-ok");
            return Ok(());
        }
    }
    anyhow::bail!("popup did not receive a valid implicit grab serial")
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProtocolViolation {
    Configure,
    Role,
}

#[derive(Default)]
struct ShellProbe {
    compositor: Option<wl_compositor::WlCompositor>,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    surface: Option<wl_surface::WlSurface>,
    xdg_surface: Option<xdg_surface::XdgSurface>,
    toplevel: Option<xdg_toplevel::XdgToplevel>,
    child_surface: Option<wl_surface::WlSurface>,
    popup_surface: Option<wl_surface::WlSurface>,
    popup: Option<xdg_popup::XdgPopup>,
    cursor_surface: Option<wl_surface::WlSurface>,
    cursor_buffer: Option<wl_buffer::WlBuffer>,
    cursor_backing_file: Option<File>,
    buffer: Option<wl_buffer::WlBuffer>,
    backing_file: Option<File>,
    configured: bool,
    configure_count: usize,
    frame_callbacks: usize,
    interaction_count: usize,
    key_events: usize,
    close_received: bool,
    last_configure_size: Option<(i32, i32)>,
    respond_to_ping: bool,
    violation: Option<ProtocolViolation>,
    violation_sent: bool,
    request_popup_grab: bool,
    popup_grab_requested: bool,
}

impl ShellProbe {
    fn initialize(&mut self, queue: &QueueHandle<Self>) {
        if self.surface.is_none()
            && let Some(compositor) = &self.compositor
        {
            self.surface = Some(compositor.create_surface(queue, ()));
        }
        if self.buffer.is_none()
            && let Some(shm) = &self.shm
        {
            let (file, buffer) =
                make_buffer(shm, queue, 160, 100).expect("create deterministic SHM buffer");
            self.backing_file = Some(file);
            self.buffer = Some(buffer);
        }
        if self.xdg_surface.is_none()
            && let (Some(wm_base), Some(surface)) = (&self.wm_base, &self.surface)
        {
            let xdg_surface = wm_base.get_xdg_surface(surface, queue, ShellSurface::Toplevel);
            let toplevel = xdg_surface.get_toplevel(queue, ());
            toplevel.set_title("nobox deterministic shell probe".to_owned());
            toplevel.set_app_id("org.nobox.shell-probe".to_owned());
            toplevel.set_min_size(120, 80);
            toplevel.set_max_size(180, 120);
            surface.commit();
            if self.violation == Some(ProtocolViolation::Role) {
                let positioner = wm_base.create_positioner(queue, ());
                positioner.set_size(20, 20);
                positioner.set_anchor_rect(0, 0, 1, 1);
                let _invalid_popup = xdg_surface.get_popup(None, &positioner, queue, ());
            }
            self.xdg_surface = Some(xdg_surface);
            self.toplevel = Some(toplevel);
        }
        if self.child_surface.is_none()
            && let (Some(compositor), Some(subcompositor), Some(parent), Some(buffer)) = (
                &self.compositor,
                &self.subcompositor,
                &self.surface,
                &self.buffer,
            )
        {
            let child = compositor.create_surface(queue, ());
            let subsurface = subcompositor.get_subsurface(&child, parent, queue, ());
            subsurface.set_position(18, 18);
            subsurface.set_desync();
            child.attach(Some(buffer), 0, 0);
            child.damage_buffer(0, 0, 48, 32);
            child.commit();
            parent.commit();
            self.child_surface = Some(child);
        }
        if self.popup.is_none()
            && let (Some(compositor), Some(wm_base), Some(parent), Some(buffer)) = (
                &self.compositor,
                &self.wm_base,
                &self.xdg_surface,
                &self.buffer,
            )
        {
            let surface = compositor.create_surface(queue, ());
            let positioner = wm_base.create_positioner(queue, ());
            positioner.set_size(80, 60);
            positioner.set_anchor_rect(130, 80, 20, 16);
            positioner.set_anchor(xdg_positioner::Anchor::BottomRight);
            positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
            positioner.set_constraint_adjustment(
                xdg_positioner::ConstraintAdjustment::FlipX
                    | xdg_positioner::ConstraintAdjustment::FlipY
                    | xdg_positioner::ConstraintAdjustment::SlideX
                    | xdg_positioner::ConstraintAdjustment::SlideY,
            );
            let popup_xdg = wm_base.get_xdg_surface(&surface, queue, ShellSurface::Popup);
            let popup = popup_xdg.get_popup(Some(parent), &positioner, queue, ());
            surface.commit();
            let _ = buffer;
            self.popup_surface = Some(surface);
            self.popup = Some(popup);
        }
        if self.violation == Some(ProtocolViolation::Configure)
            && !self.violation_sent
            && let (Some(surface), Some(buffer), Some(_xdg_surface)) =
                (&self.surface, &self.buffer, &self.xdg_surface)
        {
            surface.attach(Some(buffer), 0, 0);
            surface.commit();
            self.violation_sent = true;
        }
    }
}

fn make_buffer(
    shm: &wl_shm::WlShm,
    queue: &QueueHandle<ShellProbe>,
    width: i32,
    height: i32,
) -> Result<(File, wl_buffer::WlBuffer)> {
    let stride = width.checked_mul(4).context("SHM stride overflow")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .context("XDG_RUNTIME_DIR is unset")?;
    let path = runtime.join(format!("nobox-shell-probe-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("create {}", path.display()))?;
    std::fs::remove_file(&path).with_context(|| format!("unlink {}", path.display()))?;
    let length = usize::try_from(
        stride
            .checked_mul(height)
            .context("SHM buffer length overflow")?,
    )
    .context("SHM buffer dimensions must be positive")?;
    let mut pixels = vec![0_u8; length];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[0x30, 0x80, 0xd0, 0xff]);
    }
    file.write_all(&pixels)?;
    file.seek(SeekFrom::Start(0))?;
    let pool = shm.create_pool(file.as_fd(), i32::try_from(length)?, queue, ());
    let buffer = pool.create_buffer(
        0,
        width,
        height,
        stride,
        wl_shm::Format::Argb8888,
        queue,
        (),
    );
    pool.destroy();
    Ok((file, buffer))
}

impl Dispatch<wl_registry::WlRegistry, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(5), queue, ()));
                }
                "wl_subcompositor" => {
                    state.subcompositor = Some(registry.bind(name, version.min(1), queue, ()));
                }
                "wl_shm" => state.shm = Some(registry.bind(name, version.min(2), queue, ())),
                "xdg_wm_base" => {
                    state.wm_base = Some(registry.bind(name, version.min(6), queue, ()));
                }
                "wl_seat" => state.seat = Some(registry.bind(name, version.min(9), queue, ())),
                _ => {}
            }
            state.initialize(queue);
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            if state.respond_to_ping {
                wm_base.pong(serial);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ShellSurface {
    Toplevel,
    Popup,
}

impl Dispatch<xdg_surface::XdgSurface, ShellSurface> for ShellProbe {
    fn event(
        state: &mut Self,
        xdg_surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        data: &ShellSurface,
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.configure_count = state.configure_count.saturating_add(1);
            match data {
                ShellSurface::Toplevel => {
                    state.configured = true;
                    if let (Some(surface), Some(buffer)) = (&state.surface, &state.buffer) {
                        surface.attach(Some(buffer), 0, 0);
                        surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
                        surface.frame(queue, ());
                        surface.commit();
                    }
                }
                ShellSurface::Popup => {
                    if let (Some(surface), Some(buffer)) = (&state.popup_surface, &state.buffer) {
                        surface.attach(Some(buffer), 0, 0);
                        surface.damage_buffer(0, 0, 80, 60);
                        surface.commit();
                    }
                }
            }
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _toplevel: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event
            && width > 0
            && height > 0
        {
            state.last_configure_size = Some((width, height));
        }
        if matches!(event, xdg_toplevel::Event::Close) {
            state.close_received = true;
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _callback: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_callback::Event::Done { .. }) {
            state.frame_callbacks = state.frame_callbacks.saturating_add(1);
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
            && capabilities.contains(wl_seat::Capability::Pointer)
            && state.pointer.is_none()
        {
            state.pointer = Some(seat.get_pointer(queue, ()));
        }
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
            && capabilities.contains(wl_seat::Capability::Keyboard)
            && state.keyboard.is_none()
        {
            state.keyboard = Some(seat.get_keyboard(queue, ()));
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _keyboard: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_keyboard::Event::Key { .. }) {
            state.key_events = state.key_events.saturating_add(1);
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Enter { serial, .. } = event {
            if state.cursor_surface.is_none()
                && let (Some(compositor), Some(shm)) = (&state.compositor, &state.shm)
            {
                let (file, buffer) =
                    make_buffer(shm, queue, 16, 16).expect("create deterministic cursor buffer");
                let surface = compositor.create_surface(queue, ());
                surface.attach(Some(&buffer), 0, 0);
                surface.damage_buffer(0, 0, 16, 16);
                surface.commit();
                pointer.set_cursor(serial, Some(&surface), 1, 1);
                state.cursor_surface = Some(surface);
                state.cursor_buffer = Some(buffer);
                state.cursor_backing_file = Some(file);
            }
            return;
        }
        let wl_pointer::Event::Button {
            serial,
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        else {
            return;
        };
        let (Some(seat), Some(toplevel)) = (&state.seat, &state.toplevel) else {
            return;
        };
        if state.request_popup_grab && !state.popup_grab_requested {
            if let Some(popup) = &state.popup {
                popup.grab(seat, serial);
                state.popup_grab_requested = true;
            }
            return;
        }
        if state.interaction_count == 0 {
            toplevel._move(seat, serial);
        } else if state.interaction_count == 1 {
            toplevel.resize(seat, serial, xdg_toplevel::ResizeEdge::BottomRight);
        }
        state.interaction_count = state.interaction_count.saturating_add(1);
    }
}

fn inject_parent_input(events: &[(u8, u8, i16, i16)]) -> Result<()> {
    let (connection, screen) = x11rb::connect(None)?;
    let root = connection.setup().roots[screen].root;
    if events
        .iter()
        .any(|(type_, _, _, _)| matches!(*type_, KEY_PRESS_EVENT | KEY_RELEASE_EVENT))
    {
        let child = connection.query_pointer(root)?.reply()?.child;
        if child != 0 {
            connection
                .set_input_focus(InputFocus::PARENT, child, CURRENT_TIME)?
                .check()?;
        }
    }
    for &(type_, detail, x, y) in events {
        connection
            .xtest_fake_input(type_, detail, CURRENT_TIME, root, x, y, 0)?
            .check()?;
    }
    connection.flush()?;
    Ok(())
}

delegate_noop!(ShellProbe: ignore wl_compositor::WlCompositor);
delegate_noop!(ShellProbe: ignore wl_surface::WlSurface);
delegate_noop!(ShellProbe: ignore wl_shm::WlShm);
delegate_noop!(ShellProbe: ignore wl_shm_pool::WlShmPool);
delegate_noop!(ShellProbe: ignore wl_buffer::WlBuffer);
delegate_noop!(ShellProbe: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(ShellProbe: ignore wl_subsurface::WlSubsurface);
delegate_noop!(ShellProbe: ignore xdg_positioner::XdgPositioner);
delegate_noop!(ShellProbe: ignore xdg_popup::XdgPopup);
