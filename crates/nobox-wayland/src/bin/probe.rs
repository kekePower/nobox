//! Deterministically probe a Nobox Wayland compositor.

use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    os::fd::AsFd as _,
    os::unix::net::UnixStream,
    path::PathBuf,
};

use anyhow::{Context as _, Result, ensure};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_data_device, wl_data_device_manager,
        wl_data_offer, wl_data_source, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat,
        wl_shm, wl_shm_pool, wl_subcompositor, wl_subsurface, wl_surface, wl_touch,
    },
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1, ext_session_lock_surface_v1, ext_session_lock_v1,
};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1, ext_workspace_handle_v1, ext_workspace_manager_v1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1,
};
use wayland_protocols::wp::{
    cursor_shape::v1::client::{wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1},
    fractional_scale::v1::client::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1},
    idle_inhibit::zv1::client::{zwp_idle_inhibit_manager_v1, zwp_idle_inhibitor_v1},
    keyboard_shortcuts_inhibit::zv1::client::{
        zwp_keyboard_shortcuts_inhibit_manager_v1, zwp_keyboard_shortcuts_inhibitor_v1,
    },
    pointer_constraints::zv1::client::{
        zwp_confined_pointer_v1, zwp_locked_pointer_v1, zwp_pointer_constraints_v1,
    },
    pointer_gestures::zv1::client::{
        zwp_pointer_gesture_hold_v1, zwp_pointer_gesture_pinch_v1, zwp_pointer_gesture_swipe_v1,
        zwp_pointer_gestures_v1,
    },
    presentation_time::client::{wp_presentation, wp_presentation_feedback},
    primary_selection::zv1::client::{
        zwp_primary_selection_device_manager_v1, zwp_primary_selection_device_v1,
        zwp_primary_selection_offer_v1, zwp_primary_selection_source_v1,
    },
    relative_pointer::zv1::client::{zwp_relative_pointer_manager_v1, zwp_relative_pointer_v1},
    tablet::zv2::client::{
        zwp_tablet_manager_v2, zwp_tablet_pad_v2, zwp_tablet_seat_v2, zwp_tablet_tool_v2,
        zwp_tablet_v2,
    },
    text_input::zv3::client::{zwp_text_input_manager_v3, zwp_text_input_v3},
    viewporter::client::{wp_viewport, wp_viewporter},
};
use wayland_protocols::xdg::activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1};
use wayland_protocols::xdg::shell::client::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2, zwp_input_method_manager_v2, zwp_input_method_v2,
    zwp_input_popup_surface_v2,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1, zwlr_foreign_toplevel_manager_v1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};
use x11rb::{
    CURRENT_TIME,
    connection::Connection as _,
    protocol::{
        xproto::{
            BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, InputFocus,
            KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT, MapState,
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
        Some("--invalid-viewport") => return probe_protocol_error(ProtocolViolation::Viewport),
        Some("--invalid-fractional-scale") => {
            return probe_protocol_error(ProtocolViolation::FractionalScale);
        }
        Some("--surface-limit") => return probe_surface_limit(),
        Some("--frame-callback-limit") => return probe_core_resource_limit(CoreLimit::Callbacks),
        Some("--shm-pool-limit") => return probe_core_resource_limit(CoreLimit::ShmPools),
        Some("--shm-buffer-limit") => return probe_core_resource_limit(CoreLimit::ShmBuffers),
        Some("--shm-size-limit") => return probe_core_resource_limit(CoreLimit::ShmSize),
        Some("--shm-dimension-limit") => {
            return probe_core_resource_limit(CoreLimit::ShmDimension);
        }
        Some("--xdg-positioner-limit") => {
            return probe_core_resource_limit(CoreLimit::XdgPositioners);
        }
        Some("--core-resource-churn") => return probe_core_resource_churn(),
        Some("--xdg-popup-limit") => return probe_core_resource_limit(CoreLimit::XdgPopups),
        Some("--pending-configure-limit") => {
            return probe_core_resource_limit(CoreLimit::PendingConfigures);
        }
        Some("--wlr-foreign-manager-limit") => {
            return probe_core_resource_limit(CoreLimit::WlrForeignManagers);
        }
        Some("--surface-protocols") => return probe_surface_protocols(),
        Some("--wlr-foreign-management") => return probe_wlr_foreign_management(),
        Some("--panel-workspace-click") => return probe_panel_workspace_click(),
        Some("--panel-task-click") => return probe_panel_task_click(),
        Some("--panel-task-all-click") => return probe_panel_task_all_click(),
        Some("--panel-launcher-click") => return probe_panel_launcher_click(),
        Some("--selection") => return probe_selection(),
        Some("--selection-owner") => return probe_selection_owner(),
        Some("--selection-observer") => return probe_selection_observer(),
        Some("--xwayland-selection-observer") => return probe_xwayland_selection_observer(),
        Some("--selection-source-limit") => {
            return probe_selection_resource_limit(SelectionLimit::Sources);
        }
        Some("--selection-device-limit") => {
            return probe_selection_resource_limit(SelectionLimit::Devices);
        }
        Some("--selection-mime-limit") => {
            return probe_selection_resource_limit(SelectionLimit::MimeCount);
        }
        Some("--selection-mime-size-limit") => {
            return probe_selection_resource_limit(SelectionLimit::MimeSize);
        }
        Some("--dnd") => return probe_dnd(false, false),
        Some("--dnd-cancel") => return probe_dnd(true, false),
        Some("--dnd-xwayland-source") => return probe_dnd(false, true),
        Some("--pointer-lock") => return probe_pointer_protocols(PointerProbeMode::Lock),
        Some("--pointer-confine") => return probe_pointer_protocols(PointerProbeMode::Confine),
        Some("--pointer-constraint-duplicate") => {
            return probe_pointer_protocols(PointerProbeMode::Duplicate);
        }
        Some("--pointer-extension-limit") => return probe_pointer_extension_limit(),
        Some("--presentation") => return probe_presentation(),
        Some("--presentation-limit") => return probe_presentation_limit(),
        Some("--shortcut-inhibit") => return probe_shortcut_inhibit(),
        Some("--shortcut-inhibit-limit") => return probe_shortcut_inhibit_limit(),
        Some("--pointer-gestures") => return probe_pointer_gesture_objects(false),
        Some("--pointer-gesture-limit") => return probe_pointer_gesture_objects(true),
        Some("--cursor-shape") => return probe_cursor_shape(),
        Some("--cursor-shape-limit") => return probe_cursor_shape_limit(),
        Some("--touch") => return probe_touch_objects(false),
        Some("--touch-limit") => return probe_touch_objects(true),
        Some("--tablet") => return probe_tablet_objects(false),
        Some("--tablet-limit") => return probe_tablet_objects(true),
        Some("--input-method") => return probe_input_method(),
        Some("--text-input") => return probe_text_input(false),
        Some("--text-input-limit") => return probe_text_input(true),
        Some("--idle") => return probe_idle_lifecycle(),
        Some("--idle-inhibit-limit") => return probe_idle_limit(true),
        Some("--idle-notify-limit") => return probe_idle_limit(false),
        Some("--session-lock") => return probe_session_lock(false),
        Some("--session-lock-abandon") => return probe_session_lock(true),
        Some("--session-lock-competitor") => return probe_session_lock_competitor(),
        Some("--session-lock-invalid-unlock") => return probe_session_lock_invalid_unlock(),
        Some("--session-lock-limit") => return probe_session_lock_limit(),
        Some("--unresponsive") => return probe_unresponsive(),
        Some("--agent-hold") => return probe_agent_hold(),
        Some("--close") => return probe_close(),
        Some("--decoration-close") => return probe_decoration_close(),
        Some("--keyboard-resize") => return probe_keyboard_resize(),
        Some("--mouse-resize") => return probe_mouse_resize(),
        Some("--focus-cycle") => return probe_focus_cycle(),
        Some("--directional-cycle") => return probe_directional_cycle(),
        Some("--attention") => return probe_attention(),
        Some("--follow-mouse") => return probe_follow_mouse(),
        Some("--activation-permissive") => return probe_activation_permissive(),
        Some("--menu") => return probe_menu(),
        Some("--command-menu") => return probe_command_menu(),
        Some("--application-menu") => return probe_application_menu(),
        Some("--session-client") => return probe_session_client(false),
        Some("--session-restore") => return probe_session_client(true),
        Some("--popup-grab") => return probe_popup_grab(),
        Some("--layer-shell") => return probe_layer_shell(),
        Some("--outputs") => return probe_outputs(),
        Some("--dmabuf-import-failure") => return probe_dmabuf_import_failure(),
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

#[derive(Default)]
struct PublishedOutput {
    name: Option<String>,
    position: (i32, i32),
    current_mode: Option<(i32, i32, i32)>,
    transform: Option<wl_output::Transform>,
    scale: i32,
}

#[derive(Default)]
struct OutputProbe {
    outputs: BTreeMap<u32, PublishedOutput>,
}

fn probe_outputs() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = OutputProbe::default();
    event_queue.roundtrip(&mut state)?;
    event_queue.roundtrip(&mut state)?;
    ensure!(
        !state.outputs.is_empty(),
        "compositor published no wl_output globals"
    );
    for (id, output) in state.outputs {
        let name = output.name.unwrap_or_else(|| format!("global-{id}"));
        let mode = output.current_mode.map_or_else(
            || "unknown".to_owned(),
            |(width, height, refresh)| {
                format!(
                    "{width}x{height}@{}.{:03}",
                    refresh / 1_000,
                    refresh.abs() % 1_000
                )
            },
        );
        let transform = output.transform.map_or_else(
            || "unknown".to_owned(),
            |transform| format!("{transform:?}"),
        );
        println!(
            "output id={id} name={name} position={},{} mode={mode} transform={transform} scale={}",
            output.position.0, output.position.1, output.scale
        );
    }
    Ok(())
}

impl Dispatch<wl_registry::WlRegistry, ()> for OutputProbe {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == "wl_output" => {
                state.outputs.entry(name).or_default();
                registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), queue, name);
            }
            wl_registry::Event::GlobalRemove { name } => {
                state.outputs.remove(&name);
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct SessionLockProbe {
    manager: Option<ext_session_lock_manager_v1::ExtSessionLockManagerV1>,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    output: Option<wl_output::WlOutput>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    lock: Option<ext_session_lock_v1::ExtSessionLockV1>,
    lock_surface: Option<ext_session_lock_surface_v1::ExtSessionLockSurfaceV1>,
    surface: Option<wl_surface::WlSurface>,
    buffer: Option<wl_buffer::WlBuffer>,
    backing_file: Option<File>,
    configured: bool,
    locked: bool,
    finished: bool,
    frame_done: bool,
    keyboard_entered: bool,
    key_events: usize,
}

impl SessionLockProbe {
    fn initialize(&mut self, queue: &QueueHandle<Self>) {
        if self.lock.is_some() {
            return;
        }
        let (Some(manager), Some(compositor), Some(output)) =
            (&self.manager, &self.compositor, &self.output)
        else {
            return;
        };
        let surface = compositor.create_surface(queue, ());
        let lock = manager.lock(queue, ());
        let lock_surface = lock.get_lock_surface(&surface, output, queue, ());
        self.surface = Some(surface);
        self.lock_surface = Some(lock_surface);
        self.lock = Some(lock);
    }
}

fn probe_session_lock(abandon: bool) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = SessionLockProbe::default();
    let mut input_injected = false;
    for _ in 0..40 {
        event_queue.roundtrip(&mut state)?;
        ensure!(!state.finished, "compositor refused the session lock");
        if state.locked
            && state.configured
            && state.frame_done
            && state.keyboard_entered
            && !input_injected
        {
            inject_parent_input(&[(KEY_PRESS_EVENT, 38, 0, 0), (KEY_RELEASE_EVENT, 38, 0, 0)])?;
            input_injected = true;
        }
        if state.locked
            && state.configured
            && state.frame_done
            && (abandon || state.key_events >= 2)
        {
            if abandon {
                println!("session-lock-abandon-ok locked secure-frame");
                return Ok(());
            }
            let lock_surface = state.lock_surface.take().expect("configured lock surface");
            lock_surface.destroy();
            if let Some(surface) = state.surface.take() {
                surface.destroy();
            }
            state
                .lock
                .take()
                .expect("confirmed session lock")
                .unlock_and_destroy();
            event_queue.roundtrip(&mut state)?;
            println!("session-lock-ok secure-frame keyboard unlock");
            return Ok(());
        }
    }
    anyhow::bail!(
        "session lock incomplete: configured={} locked={} frame={} keyboard_entered={} keys={}",
        state.configured,
        state.locked,
        state.frame_done,
        state.keyboard_entered,
        state.key_events
    )
}

#[derive(Default)]
struct SessionLockControlProbe {
    manager: Option<ext_session_lock_manager_v1::ExtSessionLockManagerV1>,
    requested: usize,
    request_count: usize,
    invalid_unlock: bool,
    finished: usize,
    locks: Vec<ext_session_lock_v1::ExtSessionLockV1>,
}

fn probe_session_lock_competitor() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = SessionLockControlProbe {
        request_count: 1,
        ..SessionLockControlProbe::default()
    };
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.finished == 1 {
            println!("session-lock-competitor-ok finished");
            return Ok(());
        }
    }
    anyhow::bail!("competing session lock was not refused")
}

fn probe_session_lock_limit() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = SessionLockControlProbe {
        request_count: 9,
        ..SessionLockControlProbe::default()
    };
    for _ in 0..4 {
        if event_queue.roundtrip(&mut state).is_err() {
            println!("session-lock-limit-ok");
            return Ok(());
        }
    }
    anyhow::bail!("session-lock limit did not disconnect its client")
}

fn probe_session_lock_invalid_unlock() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = SessionLockControlProbe {
        request_count: 1,
        invalid_unlock: true,
        ..SessionLockControlProbe::default()
    };
    for _ in 0..4 {
        if event_queue.roundtrip(&mut state).is_err() {
            println!("session-lock-invalid-unlock-ok secure-disconnect");
            return Ok(());
        }
    }
    anyhow::bail!("invalid pre-confirmation unlock did not disconnect its client")
}

impl Dispatch<wl_registry::WlRegistry, ()> for SessionLockProbe {
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
                "ext_session_lock_manager_v1" => {
                    state.manager = Some(registry.bind(name, version.min(1), queue, ()))
                }
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(5), queue, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, version.min(2), queue, ())),
                "wl_output" if state.output.is_none() => {
                    state.output = Some(registry.bind(name, version.min(4), queue, ()))
                }
                "wl_seat" => state.seat = Some(registry.bind(name, version.min(9), queue, ())),
                _ => {}
            }
            state.initialize(queue);
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for SessionLockControlProbe {
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
            && interface == "ext_session_lock_manager_v1"
            && state.requested == 0
        {
            let manager: ext_session_lock_manager_v1::ExtSessionLockManagerV1 =
                registry.bind(name, version.min(1), queue, ());
            if state.invalid_unlock {
                manager.lock(queue, ()).unlock_and_destroy();
            } else {
                for _ in 0..state.request_count {
                    state.locks.push(manager.lock(queue, ()));
                }
            }
            state.requested = state.request_count;
            state.manager = Some(manager);
        }
    }
}

delegate_noop!(SessionLockProbe: ignore wl_compositor::WlCompositor);
delegate_noop!(SessionLockProbe: ignore wl_surface::WlSurface);
delegate_noop!(SessionLockProbe: ignore wl_shm::WlShm);
delegate_noop!(SessionLockProbe: ignore wl_shm_pool::WlShmPool);
delegate_noop!(SessionLockProbe: ignore wl_buffer::WlBuffer);
delegate_noop!(SessionLockProbe: ignore wl_output::WlOutput);
delegate_noop!(SessionLockProbe: ignore ext_session_lock_manager_v1::ExtSessionLockManagerV1);
delegate_noop!(SessionLockControlProbe: ignore ext_session_lock_manager_v1::ExtSessionLockManagerV1);

impl Dispatch<ext_session_lock_v1::ExtSessionLockV1, ()> for SessionLockProbe {
    fn event(
        state: &mut Self,
        _lock: &ext_session_lock_v1::ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => state.locked = true,
            ext_session_lock_v1::Event::Finished => state.finished = true,
            _ => {}
        }
    }
}

impl Dispatch<ext_session_lock_v1::ExtSessionLockV1, ()> for SessionLockControlProbe {
    fn event(
        state: &mut Self,
        _lock: &ext_session_lock_v1::ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(event, ext_session_lock_v1::Event::Finished) {
            state.finished = state.finished.saturating_add(1);
        }
    }
}

impl Dispatch<ext_session_lock_surface_v1::ExtSessionLockSurfaceV1, ()> for SessionLockProbe {
    fn event(
        state: &mut Self,
        surface: &ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        else {
            return;
        };
        surface.ack_configure(serial);
        if state.buffer.is_none() {
            let shm = state.shm.as_ref().expect("lock configure preceded wl_shm");
            let (file, buffer) = make_buffer(
                shm,
                queue,
                i32::try_from(width).expect("lock width fits i32"),
                i32::try_from(height).expect("lock height fits i32"),
            )
            .expect("create session-lock buffer");
            let wl_surface = state.surface.as_ref().expect("configured lock has surface");
            wl_surface.attach(Some(&buffer), 0, 0);
            wl_surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
            wl_surface.frame(queue, ());
            wl_surface.commit();
            state.backing_file = Some(file);
            state.buffer = Some(buffer);
        }
        state.configured = true;
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for SessionLockProbe {
    fn event(
        state: &mut Self,
        _callback: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_callback::Event::Done { .. }) {
            state.frame_done = true;
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for SessionLockProbe {
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
            && capabilities.contains(wl_seat::Capability::Keyboard)
            && state.keyboard.is_none()
        {
            state.keyboard = Some(seat.get_keyboard(queue, ()));
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for SessionLockProbe {
    fn event(
        state: &mut Self,
        _keyboard: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => state.keyboard_entered = true,
            wl_keyboard::Event::Key { .. } => {
                state.key_events = state.key_events.saturating_add(1);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for OutputProbe {
    fn event(
        state: &mut Self,
        _output: &wl_output::WlOutput,
        event: wl_output::Event,
        id: &u32,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        let Some(output) = state.outputs.get_mut(id) else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                x, y, transform, ..
            } => {
                output.position = (x, y);
                output.transform = transform.into_result().ok();
            }
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                refresh,
            } if flags.contains(wl_output::Mode::Current) => {
                output.current_mode = Some((width, height, refresh));
            }
            wl_output::Event::Scale { factor } => output.scale = factor,
            wl_output::Event::Name { name } => output.name = Some(name),
            _ => {}
        }
    }
}

#[derive(Default)]
struct DmabufFailureProbe {
    dmabuf: Option<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>,
    failed: bool,
    unexpectedly_created: bool,
}

fn probe_dmabuf_import_failure() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = DmabufFailureProbe::default();
    event_queue.roundtrip(&mut state)?;
    let dmabuf = state
        .dmabuf
        .clone()
        .context("zwp_linux_dmabuf_v1 was not advertised")?;
    let (invalid_plane, _peer) = UnixStream::pair()?;
    let params = dmabuf.create_params(&queue, ());
    params.add(invalid_plane.as_fd(), 0, 0, 64 * 4, u32::MAX, u32::MAX);
    params.create(
        64,
        64,
        smithay::backend::allocator::Fourcc::Argb8888 as u32,
        zwp_linux_buffer_params_v1::Flags::empty(),
    );
    while !state.failed && !state.unexpectedly_created {
        event_queue.blocking_dispatch(&mut state)?;
    }
    ensure!(
        !state.unexpectedly_created,
        "renderer unexpectedly imported a non-DMA-BUF socket"
    );
    println!("dmabuf-import-failure-ok");
    Ok(())
}

impl Dispatch<wl_registry::WlRegistry, ()> for DmabufFailureProbe {
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
            && interface == "zwp_linux_dmabuf_v1"
        {
            state.dmabuf = Some(registry.bind(name, version.min(3), queue, ()));
        }
    }
}

impl Dispatch<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1, ()> for DmabufFailureProbe {
    fn event(
        _state: &mut Self,
        _dmabuf: &zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
        _event: zwp_linux_dmabuf_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, ()> for DmabufFailureProbe {
    fn event(
        state: &mut Self,
        _params: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwp_linux_buffer_params_v1::Event::Created { .. } => {
                state.unexpectedly_created = true;
            }
            zwp_linux_buffer_params_v1::Event::Failed => state.failed = true,
            _ => {}
        }
    }

    wayland_client::event_created_child!(
        DmabufFailureProbe,
        zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        [
            zwp_linux_buffer_params_v1::EVT_CREATED_OPCODE => (wl_buffer::WlBuffer, ())
        ]
    );
}

delegate_noop!(DmabufFailureProbe: ignore wl_buffer::WlBuffer);

fn probe_text_input(exceed_limit: bool) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        exercise_text_input: !exceed_limit,
        text_input_limit: exceed_limit,
        ..ShellProbe::default()
    };
    for round in 0..24 {
        match event_queue.roundtrip(&mut state) {
            Ok(_) => state.initialize(&queue),
            Err(_) if exceed_limit => {
                println!("text-input-limit-ok");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
        ensure!(
            !state.saw_input_method_manager,
            "ordinary client saw the privileged input-method manager"
        );
        if round == 4 && !state.text_input_entered {
            inject_parent_input(&[
                (MOTION_NOTIFY_EVENT, 0, 300, 180),
                (BUTTON_PRESS_EVENT, 1, 300, 180),
                (BUTTON_RELEASE_EVENT, 1, 300, 180),
            ])?;
        }
        if state.text_input_commit.as_deref() == Some("nobox-ime")
            && state.text_input_done
            && state.text_input_left
        {
            println!("text-input-ok focus commit ime-death");
            return Ok(());
        }
    }
    if exceed_limit {
        anyhow::bail!("text-input limit did not disconnect its client")
    }
    anyhow::bail!(
        "text-input transaction incomplete: entered={} commit={:?} done={} left={}",
        state.text_input_entered,
        state.text_input_commit,
        state.text_input_done,
        state.text_input_left
    )
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
    ensure!(
        state.foreign_done
            && state.foreign_title.as_deref() == Some("nobox deterministic shell probe")
            && state.foreign_app_id.as_deref() == Some("org.nobox.shell-probe"),
        "foreign-toplevel publication was incomplete: title={:?} app_id={:?} done={}",
        state.foreign_title,
        state.foreign_app_id,
        state.foreign_done
    );
    ensure!(
        state.workspace_done && !state.workspaces.is_empty(),
        "ext-workspace publication was incomplete"
    );
    ensure!(
        state
            .workspaces
            .iter()
            .filter(|workspace| workspace.active)
            .count()
            == 1,
        "ext-workspace did not publish exactly one active workspace"
    );
    if state.workspaces.len() > 1 {
        let manager = state
            .workspace_manager
            .clone()
            .expect("workspace data exists");
        let first = state.workspaces[0].handle.clone();
        let second = state.workspaces[1].handle.clone();
        second.activate();
        manager.commit();
        event_queue.roundtrip(&mut state)?;
        ensure!(
            state.workspaces[1].active
                && state
                    .workspaces
                    .iter()
                    .filter(|workspace| workspace.active)
                    .count()
                    == 1,
            "atomic workspace activation was not published"
        );
        first.activate();
        manager.commit();
        event_queue.roundtrip(&mut state)?;
        ensure!(
            state.workspaces[0].active,
            "workspace restore was not published"
        );
    }
    if inject_input {
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 300, 180)])?;
        for _ in 0..2 {
            event_queue.roundtrip(&mut state)?;
        }
        inject_parent_input(&[(BUTTON_PRESS_EVENT, 1, 300, 180)])?;
        for _ in 0..3 {
            event_queue.roundtrip(&mut state)?;
        }
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 320, 195)])?;
        event_queue.roundtrip(&mut state)?;
        inject_parent_input(&[(BUTTON_RELEASE_EVENT, 1, 320, 195)])?;
        event_queue.roundtrip(&mut state)?;
        inject_parent_input(&[
            (MOTION_NOTIFY_EVENT, 0, 330, 200),
            (BUTTON_PRESS_EVENT, 1, 330, 200),
        ])?;
        for _ in 0..3 {
            event_queue.roundtrip(&mut state)?;
        }
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 395, 245)])?;
        event_queue.roundtrip(&mut state)?;
        inject_parent_input(&[(BUTTON_RELEASE_EVENT, 1, 395, 245)])?;
        event_queue.roundtrip(&mut state)?;
        inject_parent_input(&[
            (MOTION_NOTIFY_EVENT, 0, 350, 215),
            (BUTTON_PRESS_EVENT, 1, 350, 215),
        ])?;
        for _ in 0..3 {
            event_queue.roundtrip(&mut state)?;
        }
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 480, 315)])?;
        event_queue.roundtrip(&mut state)?;
        inject_parent_input(&[(BUTTON_RELEASE_EVENT, 1, 480, 315)])?;
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
        if state.workspaces.len() > 1 {
            inject_parent_input(&[
                (KEY_PRESS_EVENT, 133, 0, 0),
                (KEY_PRESS_EVENT, 114, 0, 0),
                (KEY_RELEASE_EVENT, 114, 0, 0),
                (KEY_RELEASE_EVENT, 133, 0, 0),
            ])?;
            for _ in 0..3 {
                event_queue.roundtrip(&mut state)?;
            }
            ensure!(
                state.workspaces[1].active,
                "default Super-Right binding did not switch core workspace"
            );
            inject_parent_input(&[
                (KEY_PRESS_EVENT, 133, 0, 0),
                (KEY_PRESS_EVENT, 113, 0, 0),
                (KEY_RELEASE_EVENT, 113, 0, 0),
                (KEY_RELEASE_EVENT, 133, 0, 0),
            ])?;
            for _ in 0..3 {
                event_queue.roundtrip(&mut state)?;
            }
            ensure!(
                state.workspaces[0].active,
                "default Super-Left binding did not restore core workspace"
            );
        }
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

    if inject_input {
        let (Some(activation), Some(seat), Some(surface), Some(serial)) = (
            &state.activation,
            &state.seat,
            &state.surface,
            state.last_input_serial,
        ) else {
            anyhow::bail!("xdg activation prerequisites were not advertised");
        };
        let token = activation.get_activation_token(&queue, ());
        token.set_serial(serial, seat);
        token.set_surface(surface);
        token.set_app_id("org.nobox.Probe".to_owned());
        token.commit();
        state.activation_token = Some(token);
        for _ in 0..2 {
            event_queue.roundtrip(&mut state)?;
        }
        ensure!(
            state.activation_done,
            "xdg activation token was not completed"
        );
        let keys_before_activation = state.key_events;
        inject_parent_input(&[(KEY_PRESS_EVENT, 38, 0, 0), (KEY_RELEASE_EVENT, 38, 0, 0)])?;
        event_queue.roundtrip(&mut state)?;
        ensure!(
            state.key_events >= keys_before_activation.saturating_add(2),
            "valid xdg activation did not restore keyboard focus"
        );
    }

    if let Some(surface) = &state.surface {
        surface.attach(None, 0, 0);
        surface.commit();
    }
    for _ in 0..2 {
        event_queue.roundtrip(&mut state)?;
        if state.foreign_closed {
            break;
        }
    }
    ensure!(
        state.foreign_closed,
        "unmapping the toplevel did not close its foreign-toplevel handle"
    );
    println!(
        "shell-ok configures={} frames={}",
        state.configure_count, state.frame_callbacks
    );
    Ok(())
}

fn probe_wlr_foreign_management() -> Result<()> {
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
        if state.configured && state.wlr_foreign_done {
            break;
        }
    }
    let handle = state
        .wlr_foreign_handle
        .clone()
        .context("wlr foreign-toplevel handle was not published")?;
    ensure!(
        state.wlr_foreign_title.as_deref() == Some("nobox deterministic shell probe")
            && state.wlr_foreign_app_id.as_deref() == Some("org.nobox.shell-probe"),
        "wlr foreign-toplevel metadata was incomplete"
    );
    handle.set_minimized();
    event_queue.roundtrip(&mut state)?;
    ensure!(
        state.wlr_foreign_minimized,
        "minimize request was not applied"
    );
    handle.unset_minimized();
    let seat = state.seat.clone().context("wl_seat was not advertised")?;
    handle.activate(&seat);
    event_queue.roundtrip(&mut state)?;
    ensure!(
        state.wlr_foreign_activated && !state.wlr_foreign_minimized,
        "activate/unminimize requests were not applied"
    );
    handle.close();
    event_queue.roundtrip(&mut state)?;
    ensure!(
        state.close_received,
        "close request did not reach xdg-toplevel"
    );
    println!("wlr-foreign-management-ok minimize activate close");
    Ok(())
}

fn probe_panel_workspace_click() -> Result<()> {
    let (connection, mut event_queue, mut state) = connected_shell_probe()?;
    let _ = connection;
    ensure!(
        state.workspaces.len() >= 2,
        "panel test needs two workspaces"
    );
    inject_parent_surface_input(&[
        (MOTION_NOTIFY_EVENT, 0, 45, 15),
        (BUTTON_PRESS_EVENT, 1, 45, 15),
        (BUTTON_RELEASE_EVENT, 1, 45, 15),
    ])?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.workspaces[1].active {
            println!("panel-workspace-click-ok");
            return Ok(());
        }
    }
    anyhow::bail!("Wayland panel workspace button did not activate workspace two")
}

fn probe_panel_task_click() -> Result<()> {
    let (_connection, mut event_queue, mut state) = connected_shell_probe()?;
    ensure!(
        state.wlr_foreign_handle.is_some(),
        "panel test task was not published"
    );
    ensure!(
        state.wlr_foreign_outputs > 0,
        "current-workspace task had no output association"
    );
    ensure!(
        state.workspaces.len() >= 2,
        "panel test needs two workspaces"
    );
    let manager = state
        .workspace_manager
        .clone()
        .context("workspace manager was not advertised")?;
    state.workspaces[1].handle.activate();
    manager.commit();
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.workspaces[1].active && state.wlr_foreign_outputs == 0 {
            break;
        }
    }
    ensure!(
        state.workspaces[1].active && state.wlr_foreign_outputs == 0,
        "hidden-workspace task retained an output association"
    );
    inject_parent_surface_input(&[
        (MOTION_NOTIFY_EVENT, 0, 100, 15),
        (MOTION_NOTIFY_EVENT, 0, 30, 15),
    ])?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    event_queue.roundtrip(&mut state)?;
    inject_parent_surface_input(&[
        (BUTTON_PRESS_EVENT, 1, 30, 15),
        (BUTTON_RELEASE_EVENT, 1, 30, 15),
    ])?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    event_queue.roundtrip(&mut state)?;
    ensure!(
        !state.wlr_foreign_minimized && state.workspaces[1].active,
        "current-workspace panel exposed a hidden task"
    );
    state.workspaces[0].handle.activate();
    manager.commit();
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.workspaces[0].active && state.wlr_foreign_outputs > 0 {
            break;
        }
    }
    ensure!(
        state.workspaces[0].active && state.wlr_foreign_outputs > 0,
        "restored current-workspace task did not regain its output association"
    );
    inject_parent_surface_input(&[
        (MOTION_NOTIFY_EVENT, 0, 100, 15),
        (MOTION_NOTIFY_EVENT, 0, 30, 15),
        (BUTTON_PRESS_EVENT, 1, 30, 15),
        (BUTTON_RELEASE_EVENT, 1, 30, 15),
    ])?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.wlr_foreign_minimized {
            break;
        }
    }
    ensure!(
        state.wlr_foreign_minimized,
        "Wayland panel task button did not minimize the active task"
    );
    inject_parent_surface_input(&[
        (BUTTON_PRESS_EVENT, 1, 30, 15),
        (BUTTON_RELEASE_EVENT, 1, 30, 15),
    ])?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.wlr_foreign_activated && !state.wlr_foreign_minimized {
            break;
        }
    }
    ensure!(
        state.wlr_foreign_activated && !state.wlr_foreign_minimized,
        "Wayland panel task button did not restore and activate the task"
    );
    inject_parent_surface_input(&[
        (BUTTON_PRESS_EVENT, 3, 30, 15),
        (BUTTON_RELEASE_EVENT, 3, 30, 15),
    ])?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.close_received {
            println!("panel-task-click-ok minimize activate close");
            return Ok(());
        }
    }
    anyhow::bail!("Wayland panel task button did not request close")
}

fn probe_panel_task_all_click() -> Result<()> {
    let (_connection, mut event_queue, mut state) = connected_shell_probe()?;
    ensure!(
        state.workspaces.len() >= 2,
        "panel test needs two workspaces"
    );
    let manager = state
        .workspace_manager
        .clone()
        .context("workspace manager was not advertised")?;
    state.workspaces[1].handle.activate();
    manager.commit();
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.workspaces[1].active && state.wlr_foreign_outputs == 0 {
            break;
        }
    }
    ensure!(
        state.workspaces[1].active && state.wlr_foreign_outputs == 0,
        "all-workspaces fixture did not hide its task first"
    );
    inject_parent_surface_input(&[
        (MOTION_NOTIFY_EVENT, 0, 100, 15),
        (MOTION_NOTIFY_EVENT, 0, 30, 15),
    ])?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    event_queue.roundtrip(&mut state)?;
    inject_parent_surface_input(&[
        (BUTTON_PRESS_EVENT, 1, 30, 15),
        (BUTTON_RELEASE_EVENT, 1, 30, 15),
    ])?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.workspaces[0].active && state.wlr_foreign_activated {
            break;
        }
    }
    ensure!(
        state.workspaces[0].active && state.wlr_foreign_activated,
        "all-workspaces panel did not expose and activate the hidden task"
    );
    if let Some(handle) = &state.wlr_foreign_handle {
        handle.close();
    }
    println!("panel-task-all-click-ok");
    Ok(())
}

fn probe_panel_launcher_click() -> Result<()> {
    inject_parent_surface_input(&[
        (MOTION_NOTIFY_EVENT, 0, 100, 15),
        (MOTION_NOTIFY_EVENT, 0, 30, 15),
        (BUTTON_PRESS_EVENT, 1, 30, 15),
        (BUTTON_RELEASE_EVENT, 1, 30, 15),
    ])?;
    println!("panel-launcher-click-ok");
    Ok(())
}

fn probe_session_client(verify_restore: bool) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        ..ShellProbe::default()
    };
    for _ in 0..8 {
        event_queue.roundtrip(&mut state)?;
        if state.configured && state.frame_callbacks > 0 && state.workspace_done {
            break;
        }
    }
    ensure!(state.configured, "session probe did not map");
    if verify_restore {
        ensure!(
            state
                .workspaces
                .get(1)
                .is_some_and(|workspace| workspace.active),
            "restored session did not select the second workspace"
        );
        let restored = state
            .last_configure_size
            .context("restored client received no saved-size configure")?;
        ensure!(
            restored.0 > 160 && (80..=120).contains(&restored.1),
            "restored client size was {restored:?}; expected saved constrained growth"
        );
        println!("session-restore-ok size={restored:?}");
        return Ok(());
    }

    inject_parent_input(&[(KEY_PRESS_EVENT, 38, 0, 0), (KEY_RELEASE_EVENT, 38, 0, 0)])?;
    println!("session-client-ready");
    std::io::stdout().flush()?;
    while event_queue.blocking_dispatch(&mut state).is_ok() {}
    println!("session-client-disconnected");
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

fn probe_surface_protocols() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        exercise_surface_protocols: true,
        ..ShellProbe::default()
    };
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.configured && state.frame_callbacks > 0 && state.preferred_scale.is_some() {
            break;
        }
    }
    ensure!(state.configured, "surface protocol probe did not map");
    ensure!(
        state.frame_callbacks > 0,
        "viewport surface received no frame callback"
    );
    ensure!(
        state.viewport.is_some(),
        "wp_viewporter did not create a viewport"
    );
    ensure!(
        state.preferred_scale == Some(120),
        "fractional scale was {:?}; expected 120 units for scale 1",
        state.preferred_scale
    );
    println!("surface-protocols-ok preferred-scale=120");
    Ok(())
}

fn probe_selection() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        exercise_selection: true,
        ..ShellProbe::default()
    };
    for _ in 0..10 {
        event_queue.roundtrip(&mut state)?;
        state.poll_selection()?;
        if state.clipboard_received && state.primary_received && !state.selection_replaced {
            state.replace_selection(&queue);
        }
        if state.clipboard_cancelled && state.primary_cancelled {
            break;
        }
    }
    ensure!(state.configured, "selection probe did not map");
    ensure!(
        state.clipboard_received,
        "clipboard payload did not round trip"
    );
    ensure!(
        state.primary_received,
        "primary-selection payload did not round trip"
    );
    ensure!(
        state.clipboard_cancelled && state.primary_cancelled,
        "replaced selection owners were not cancelled"
    );
    println!("selection-ok clipboard primary cancellation");
    Ok(())
}

fn probe_selection_owner() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        exercise_selection: true,
        ..ShellProbe::default()
    };
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.configured && state.data_source.is_some() && state.primary_source.is_some() {
            break;
        }
    }
    ensure!(state.configured, "selection owner did not map");
    ensure!(
        state.data_source.is_some() && state.primary_source.is_some(),
        "selection owner did not publish both selections"
    );
    println!("selection-owner-ready");
    std::io::stdout().flush()?;
    while event_queue.blocking_dispatch(&mut state).is_ok() {}
    Ok(())
}

fn probe_selection_observer() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        ..ShellProbe::default()
    };
    for _ in 0..10 {
        event_queue.roundtrip(&mut state)?;
        state.poll_selection()?;
        if state.clipboard_received && state.primary_received {
            break;
        }
    }
    ensure!(
        state.clipboard_received && state.primary_received,
        "observer did not receive both owner selections"
    );
    println!("selection-observer-ready");
    std::io::stdout().flush()?;
    for _ in 0..20 {
        event_queue.blocking_dispatch(&mut state)?;
        if state.clipboard_cleared && state.primary_cleared {
            println!("selection-owner-death-ok");
            return Ok(());
        }
    }
    anyhow::bail!("dead owner selections were not cleared")
}

fn probe_xwayland_selection_observer() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        ..ShellProbe::default()
    };
    for _ in 0..10 {
        event_queue.roundtrip(&mut state)?;
        state.poll_selection_payloads(b"xwayland-selection", b"xwayland-selection")?;
        if state.clipboard_received && state.primary_received {
            break;
        }
    }
    ensure!(
        state.clipboard_received && state.primary_received,
        "native observer did not receive both XWayland selections"
    );
    println!("xwayland-selection-observer-ready");
    std::io::stdout().flush()?;
    for _ in 0..20 {
        event_queue.blocking_dispatch(&mut state)?;
        if state.clipboard_cleared && state.primary_cleared {
            println!("xwayland-selection-owner-death-ok");
            return Ok(());
        }
    }
    anyhow::bail!("dead XWayland owner selections were not cleared")
}

fn probe_dnd(expect_cancel: bool, xwayland_target: bool) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        exercise_dnd: true,
        xwayland_dnd_source: xwayland_target,
        ..ShellProbe::default()
    };
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.configured && state.frame_callbacks > 0 {
            break;
        }
    }
    ensure!(state.configured, "DND probe did not map");
    let source_x = std::env::var("NOBOX_DND_SOURCE_X")
        .ok()
        .and_then(|value| value.parse::<i16>().ok())
        .unwrap_or(320);
    let source_y = std::env::var("NOBOX_DND_SOURCE_Y")
        .ok()
        .and_then(|value| value.parse::<i16>().ok())
        .unwrap_or(180);
    inject_dnd_parent_input(
        xwayland_target,
        &[
            (
                MOTION_NOTIFY_EVENT,
                0,
                source_x.saturating_sub(1),
                source_y.saturating_sub(1),
            ),
            (MOTION_NOTIFY_EVENT, 0, source_x, source_y),
        ],
    )?;
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.pointer_position.is_some() {
            break;
        }
    }
    ensure!(
        state.pointer_position.is_some(),
        "pointer did not enter the DND source"
    );
    let icon_frame_before = state.frame_callbacks;
    inject_dnd_parent_input(
        xwayland_target,
        &[(BUTTON_PRESS_EVENT, 1, source_x, source_y)],
    )?;
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.dnd_started {
            break;
        }
    }
    ensure!(state.dnd_started, "pointer press did not start a drag");

    let target_x = if xwayland_target {
        std::env::var("NOBOX_DND_TARGET_X")?.parse::<i16>()?
    } else {
        330
    };
    let target_y = if xwayland_target {
        std::env::var("NOBOX_DND_TARGET_Y")?.parse::<i16>()?
    } else {
        190
    };
    inject_dnd_parent_input(
        xwayland_target,
        &[(MOTION_NOTIFY_EVENT, 0, target_x, target_y)],
    )?;
    if xwayland_target {
        for step in 0..50 {
            event_queue.roundtrip(&mut state)?;
            std::thread::sleep(std::time::Duration::from_millis(20));
            inject_dnd_parent_input(
                xwayland_target,
                &[(
                    MOTION_NOTIFY_EVENT,
                    0,
                    target_x.saturating_add(i16::try_from(step % 2).unwrap_or_default()),
                    target_y,
                )],
            )?;
        }
        inject_dnd_parent_input(
            xwayland_target,
            &[(BUTTON_RELEASE_EVENT, 1, target_x, target_y)],
        )?;
        for _ in 0..50 {
            event_queue.roundtrip(&mut state)?;
            if state.dnd_finished {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        ensure!(
            state.dnd_drop_performed,
            "XWayland target did not accept the drag"
        );
        ensure!(
            state.dnd_finished,
            "XWayland target did not finish the drag"
        );
        println!("dnd-xwayland-source-ok");
        return Ok(());
    }
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.dnd_entered
            && state.dnd_action == Some(wl_data_device_manager::DndAction::Copy)
            && state.frame_callbacks > icon_frame_before
        {
            break;
        }
        if state.dnd_entered {
            inject_dnd_parent_input(xwayland_target, &[(MOTION_NOTIFY_EVENT, 0, 331, 191)])?;
        }
    }
    ensure!(state.dnd_entered, "drag did not enter a client surface");
    ensure!(
        state.dnd_mime_offered,
        "DND offer omitted the advertised MIME type"
    );
    ensure!(
        state
            .dnd_source_actions
            .is_some_and(|actions| actions.contains(wl_data_device_manager::DndAction::Copy)),
        "DND offer omitted the source Copy action"
    );
    ensure!(
        state.dnd_action == Some(wl_data_device_manager::DndAction::Copy),
        "drag did not negotiate the Copy action"
    );
    ensure!(
        state.frame_callbacks > icon_frame_before,
        "DND icon received no rendered frame callback"
    );

    let release_position = if expect_cancel { (10, 10) } else { (330, 190) };
    if expect_cancel {
        inject_dnd_parent_input(xwayland_target, &[(MOTION_NOTIFY_EVENT, 0, 10, 10)])?;
        event_queue.roundtrip(&mut state)?;
    }
    inject_dnd_parent_input(
        xwayland_target,
        &[(
            BUTTON_RELEASE_EVENT,
            1,
            release_position.0,
            release_position.1,
        )],
    )?;
    for _ in 0..10 {
        event_queue.roundtrip(&mut state)?;
        state.poll_dnd()?;
        if (expect_cancel && state.dnd_cancelled)
            || (!expect_cancel && state.dnd_finished && state.dnd_received)
        {
            break;
        }
    }

    if expect_cancel {
        ensure!(
            state.dnd_cancelled,
            "releasing outside a target did not cancel DND"
        );
        ensure!(
            !state.dnd_dropped,
            "cancelled DND unexpectedly delivered a drop"
        );
        println!("dnd-cancel-ok");
    } else {
        ensure!(state.dnd_dropped, "target received no DND drop");
        ensure!(
            state.dnd_drop_performed,
            "source received no DND drop-performed event"
        );
        ensure!(state.dnd_finished, "source received no DND finished event");
        ensure!(state.dnd_received, "DND payload did not round trip");
        println!("dnd-ok copy transfer drop finish icon-frame");
    }
    Ok(())
}

fn inject_dnd_parent_input(xwayland_target: bool, events: &[(u8, u8, i16, i16)]) -> Result<()> {
    if xwayland_target {
        inject_parent_surface_input(events)
    } else {
        inject_parent_input(events)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PointerProbeMode {
    Lock,
    Confine,
    Duplicate,
}

fn probe_pointer_protocols(mode: PointerProbeMode) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        pointer_probe_mode: Some(mode),
        ..ShellProbe::default()
    };
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        if state.configured && state.frame_callbacks > 0 {
            break;
        }
    }
    ensure!(state.configured, "pointer protocol probe did not map");
    let preferred = [(320, 180), (160, 130), (480, 250)];
    let grid = (10..360)
        .step_by(30)
        .flat_map(|y| (10..640).step_by(30).map(move |x| (x, y)));
    for (x, y) in preferred.into_iter().chain(grid) {
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, x, y)])?;
        for _ in 0..3 {
            match event_queue.roundtrip(&mut state) {
                Err(_) if mode == PointerProbeMode::Duplicate => {
                    println!("pointer-constraint-duplicate-ok");
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
            if state.constraint_active {
                break;
            }
        }
        if state.constraint_active {
            break;
        }
    }
    if mode == PointerProbeMode::Duplicate {
        anyhow::bail!("duplicate pointer constraint did not disconnect its client");
    }
    ensure!(
        state.constraint_active,
        "pointer constraint did not activate"
    );
    let before = state
        .pointer_position
        .context("constrained pointer received no surface position")?;
    let relative_before = state.relative_motion_count;
    let target = if mode == PointerProbeMode::Lock {
        (360, 200)
    } else {
        (790, 590)
    };
    inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, target.0, target.1)])?;
    for _ in 0..3 {
        event_queue.roundtrip(&mut state)?;
    }
    ensure!(
        state.relative_motion_count > relative_before,
        "constrained movement produced no relative-pointer event"
    );
    ensure!(
        state.pointer_position == Some(before),
        "constraint allowed the wl_pointer position to escape: {:?} -> {:?}",
        before,
        state.pointer_position
    );

    if mode == PointerProbeMode::Lock {
        let locked = state
            .locked_pointer
            .take()
            .context("lock object disappeared")?;
        locked.set_cursor_position_hint(before.0, before.1);
        state
            .constraint_surface
            .as_ref()
            .context("lock constraint surface disappeared")?
            .commit();
        state
            .surface
            .as_ref()
            .context("lock probe toplevel surface disappeared")?
            .commit();
        event_queue.roundtrip(&mut state)?;
        locked.destroy();
        for _ in 0..2 {
            event_queue.roundtrip(&mut state)?;
        }
        inject_parent_input(&[(
            MOTION_NOTIFY_EVENT,
            0,
            target.0.saturating_add(1),
            target.1.saturating_add(1),
        )])?;
        for _ in 0..3 {
            event_queue.roundtrip(&mut state)?;
        }
        let restored = state
            .pointer_position
            .context("unlocked pointer received no restored position")?;
        ensure!(
            (restored.0 - (before.0 + 1.0)).abs() <= 2.0
                && (restored.1 - (before.1 + 1.0)).abs() <= 2.0,
            "cursor hint was not applied on unlock: {restored:?}"
        );
        println!("pointer-lock-ok relative hint");
    } else {
        state
            .confined_pointer
            .take()
            .context("confine object disappeared")?
            .destroy();
        for _ in 0..2 {
            event_queue.roundtrip(&mut state)?;
        }
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 0, 0)])?;
        for _ in 0..3 {
            event_queue.roundtrip(&mut state)?;
        }
        ensure!(
            state.pointer_left,
            "destroyed pointer confinement continued to hold surface focus"
        );
        println!("pointer-confine-ok relative boundary");
    }
    Ok(())
}

fn probe_pointer_extension_limit() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        pointer_extension_limit: true,
        ..ShellProbe::default()
    };
    for _ in 0..8 {
        match event_queue.roundtrip(&mut state) {
            Ok(_) => state.initialize(&queue),
            Err(_) => {
                println!("pointer-extension-limit-ok");
                return Ok(());
            }
        }
    }
    anyhow::bail!("pointer extension object limit did not disconnect its client")
}

fn probe_presentation() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        exercise_presentation: true,
        ..ShellProbe::default()
    };
    for _ in 0..12 {
        event_queue.roundtrip(&mut state)?;
        state.initialize(&queue);
        if state.presentation_presented {
            break;
        }
    }
    ensure!(state.configured, "presentation probe did not map");
    ensure!(
        state.presentation_clock_id == Some(rustix::time::ClockId::Monotonic as u32),
        "presentation clock is not CLOCK_MONOTONIC"
    );
    ensure!(
        state.presentation_presented,
        "presentation feedback was not completed"
    );
    ensure!(
        !state.presentation_discarded,
        "presented feedback was discarded"
    );
    ensure!(
        state.presentation_refresh > 0,
        "presentation feedback omitted fixed refresh"
    );
    ensure!(
        state.presentation_sequence > 0,
        "presentation feedback omitted its sequence"
    );
    println!("presentation-ok monotonic refresh sequence");
    Ok(())
}

fn probe_presentation_limit() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        presentation_limit: true,
        ..ShellProbe::default()
    };
    for _ in 0..8 {
        match event_queue.roundtrip(&mut state) {
            Ok(_) => state.initialize(&queue),
            Err(_) => {
                println!("presentation-limit-ok");
                return Ok(());
            }
        }
    }
    anyhow::bail!("presentation feedback limit did not disconnect its client")
}

fn probe_shortcut_inhibit() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        exercise_shortcut_inhibit: true,
        ..ShellProbe::default()
    };
    for _ in 0..12 {
        event_queue.roundtrip(&mut state)?;
        state.initialize(&queue);
        if state.shortcut_inhibitor_active {
            break;
        }
    }
    ensure!(
        state.shortcut_inhibitor_active,
        "shortcut inhibitor was not activated"
    );
    let key_events_before = state.key_events;
    inject_parent_input(&[
        (KEY_PRESS_EVENT, 133, 0, 0),
        (KEY_PRESS_EVENT, 24, 0, 0),
        (KEY_RELEASE_EVENT, 24, 0, 0),
        (KEY_RELEASE_EVENT, 133, 0, 0),
    ])?;
    for _ in 0..3 {
        event_queue.roundtrip(&mut state)?;
    }
    ensure!(
        state.key_events >= key_events_before + 4,
        "inhibited shortcut was not forwarded"
    );
    ensure!(
        state.keycodes.contains(&16),
        "client did not receive inhibited Super-q"
    );
    ensure!(
        !state.close_received,
        "inhibited Super-q reached compositor policy"
    );
    state
        .shortcut_inhibitor
        .take()
        .context("shortcut inhibitor disappeared")?
        .destroy();
    event_queue.roundtrip(&mut state)?;
    inject_parent_input(&[
        (KEY_PRESS_EVENT, 133, 0, 0),
        (KEY_PRESS_EVENT, 24, 0, 0),
        (KEY_RELEASE_EVENT, 24, 0, 0),
        (KEY_RELEASE_EVENT, 133, 0, 0),
    ])?;
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.close_received {
            break;
        }
    }
    ensure!(
        state.close_received,
        "destroying inhibitor did not restore Super-q"
    );
    println!("shortcut-inhibit-ok forward restore");
    Ok(())
}

fn probe_shortcut_inhibit_limit() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        shortcut_inhibitor_limit: true,
        ..ShellProbe::default()
    };
    for _ in 0..8 {
        match event_queue.roundtrip(&mut state) {
            Ok(_) => state.initialize(&queue),
            Err(_) => {
                println!("shortcut-inhibit-limit-ok");
                return Ok(());
            }
        }
    }
    anyhow::bail!("shortcut inhibitor limit did not disconnect its client")
}

fn probe_idle_lifecycle() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        exercise_idle: true,
        ..ShellProbe::default()
    };
    for _ in 0..6 {
        event_queue.roundtrip(&mut state)?;
        state.initialize(&queue);
        if state.configured && state.idle_inhibitor.is_some() && state.idle_notification.is_some() {
            break;
        }
    }
    ensure!(state.configured, "idle probe surface did not map");
    ensure!(
        state.idle_inhibitor.is_some()
            && state.idle_notification.is_some()
            && state.input_idle_notification.is_some(),
        "idle protocols were not advertised"
    );

    std::thread::sleep(std::time::Duration::from_millis(400));
    for _ in 0..2 {
        event_queue.roundtrip(&mut state)?;
    }
    ensure!(
        state.input_idled,
        "input-only idle notification did not ignore the inhibitor"
    );
    ensure!(
        !state.standard_idled,
        "visible inhibitor did not suppress ordinary idle notification"
    );

    state
        .idle_inhibitor
        .take()
        .expect("checked above")
        .destroy();
    event_queue.roundtrip(&mut state)?;
    std::thread::sleep(std::time::Duration::from_millis(400));
    for _ in 0..2 {
        event_queue.roundtrip(&mut state)?;
    }
    ensure!(
        state.standard_idled,
        "destroying the inhibitor did not restart the idle deadline"
    );

    inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 300, 180)])?;
    for _ in 0..2 {
        event_queue.roundtrip(&mut state)?;
    }
    ensure!(
        state.standard_resumed && state.input_resumed,
        "user activity did not resume both idle notification classes"
    );
    ensure!(
        !state.standard_idled && !state.input_idled,
        "resumed notifications retained idle state"
    );
    println!("idle-ok inhibit input-idle resume");
    Ok(())
}

fn probe_idle_limit(inhibitors: bool) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        idle_inhibitor_limit: inhibitors,
        idle_notification_limit: !inhibitors,
        ..ShellProbe::default()
    };
    for _ in 0..8 {
        match event_queue.roundtrip(&mut state) {
            Ok(_) => state.initialize(&queue),
            Err(_) => {
                println!(
                    "idle-{}-limit-ok",
                    if inhibitors {
                        "inhibitor"
                    } else {
                        "notification"
                    }
                );
                return Ok(());
            }
        }
    }
    anyhow::bail!(
        "idle {} limit did not disconnect its client",
        if inhibitors {
            "inhibitor"
        } else {
            "notification"
        }
    )
}

fn probe_pointer_gesture_objects(exceed_limit: bool) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        exercise_pointer_gestures: true,
        pointer_gesture_limit: exceed_limit,
        ..ShellProbe::default()
    };
    for _ in 0..8 {
        match event_queue.roundtrip(&mut state) {
            Ok(_) => state.initialize(&queue),
            Err(_) if exceed_limit => {
                println!("pointer-gesture-limit-ok");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
        if !exceed_limit
            && state.pointer_swipe_gestures.len()
                + state.pointer_pinch_gestures.len()
                + state.pointer_hold_gestures.len()
                == 3
        {
            println!("pointer-gestures-ok swipe pinch hold");
            return Ok(());
        }
    }
    if exceed_limit {
        anyhow::bail!("pointer gesture limit did not disconnect its client")
    }
    anyhow::bail!("pointer gesture objects were not created")
}

fn probe_cursor_shape() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        exercise_cursor_shape: true,
        ..ShellProbe::default()
    };
    for _ in 0..8 {
        event_queue.roundtrip(&mut state)?;
        state.initialize(&queue);
        if state.configured && state.cursor_shape_device.is_some() {
            break;
        }
    }
    ensure!(state.configured, "cursor-shape probe did not map");
    let preferred = [(320, 180), (160, 130), (480, 250)];
    let grid = (10..360)
        .step_by(30)
        .flat_map(|y| (10..640).step_by(30).map(move |x| (x, y)));
    for (x, y) in preferred.into_iter().chain(grid) {
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, x, y)])?;
        event_queue.roundtrip(&mut state)?;
        if state.pointer_enter_serial.is_some() {
            break;
        }
    }
    let serial = state
        .pointer_enter_serial
        .context("cursor-shape probe received no pointer enter serial")?;
    let device = state
        .cursor_shape_device
        .clone()
        .context("cursor-shape device was not created")?;
    device.set_shape(serial, wp_cursor_shape_device_v1::Shape::Text);
    for _ in 0..3 {
        event_queue.roundtrip(&mut state)?;
    }
    device.set_shape(serial, wp_cursor_shape_device_v1::Shape::EwResize);
    for _ in 0..3 {
        event_queue.roundtrip(&mut state)?;
    }
    println!("cursor-shape-ok text ew-resize");
    Ok(())
}

fn probe_cursor_shape_limit() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        cursor_shape_limit: true,
        ..ShellProbe::default()
    };
    for _ in 0..8 {
        match event_queue.roundtrip(&mut state) {
            Ok(_) => state.initialize(&queue),
            Err(_) => {
                println!("cursor-shape-limit-ok");
                return Ok(());
            }
        }
    }
    anyhow::bail!("cursor-shape device limit did not disconnect its client")
}

#[derive(Default)]
struct TouchProbe {
    seat: Option<wl_seat::WlSeat>,
    touch_capability: bool,
    exceed_limit: bool,
    touches: Vec<wl_touch::WlTouch>,
}

fn probe_touch_objects(exceed_limit: bool) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = TouchProbe {
        exceed_limit,
        ..TouchProbe::default()
    };
    for _ in 0..8 {
        match event_queue.roundtrip(&mut state) {
            Ok(_) if !exceed_limit && !state.touches.is_empty() => {
                ensure!(state.touch_capability, "wl_seat did not advertise touch");
                println!("touch-ok capability device");
                return Ok(());
            }
            Ok(_) => {}
            Err(_) if exceed_limit => {
                println!("touch-limit-ok");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
    }
    if exceed_limit {
        anyhow::bail!("touch device limit did not disconnect its client")
    }
    anyhow::bail!("touch device was not created")
}

impl Dispatch<wl_registry::WlRegistry, ()> for TouchProbe {
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
            && interface == "wl_seat"
        {
            state.seat = Some(registry.bind(name, version.min(9), queue, ()));
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for TouchProbe {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };
        state.touch_capability = capabilities.contains(wl_seat::Capability::Touch);
        if !state.touch_capability || !state.touches.is_empty() {
            return;
        }
        let count = if state.exceed_limit { 17 } else { 1 };
        state.touches = (0..count).map(|_| seat.get_touch(queue, ())).collect();
    }
}

delegate_noop!(TouchProbe: ignore wl_touch::WlTouch);

#[derive(Default)]
struct TabletProbe {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<zwp_tablet_manager_v2::ZwpTabletManagerV2>,
    exceed_limit: bool,
    tablet_seats: Vec<zwp_tablet_seat_v2::ZwpTabletSeatV2>,
}

fn probe_tablet_objects(exceed_limit: bool) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = TabletProbe {
        exceed_limit,
        ..TabletProbe::default()
    };
    for _ in 0..8 {
        match event_queue.roundtrip(&mut state) {
            Ok(_) => state.initialize(&queue),
            Err(_) if exceed_limit => {
                println!("tablet-limit-ok");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
        if !exceed_limit && !state.tablet_seats.is_empty() {
            println!("tablet-ok manager seat");
            return Ok(());
        }
    }
    if exceed_limit {
        anyhow::bail!("tablet-seat limit did not disconnect its client")
    }
    anyhow::bail!("tablet seat was not created")
}

impl TabletProbe {
    fn initialize(&mut self, queue: &QueueHandle<Self>) {
        if !self.tablet_seats.is_empty() {
            return;
        }
        let (Some(manager), Some(seat)) = (&self.manager, &self.seat) else {
            return;
        };
        let count = if self.exceed_limit { 17 } else { 1 };
        self.tablet_seats = (0..count)
            .map(|_| manager.get_tablet_seat(seat, queue, ()))
            .collect();
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for TabletProbe {
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
                "wl_seat" => state.seat = Some(registry.bind(name, version.min(9), queue, ())),
                "zwp_tablet_manager_v2" => {
                    state.manager = Some(registry.bind(name, version.min(1), queue, ()))
                }
                _ => {}
            }
        }
    }
}

delegate_noop!(TabletProbe: ignore wl_seat::WlSeat);
delegate_noop!(TabletProbe: ignore zwp_tablet_manager_v2::ZwpTabletManagerV2);

impl Dispatch<zwp_tablet_seat_v2::ZwpTabletSeatV2, ()> for TabletProbe {
    fn event(
        _state: &mut Self,
        _seat: &zwp_tablet_seat_v2::ZwpTabletSeatV2,
        _event: zwp_tablet_seat_v2::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(TabletProbe, zwp_tablet_seat_v2::ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (zwp_tablet_v2::ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (zwp_tablet_tool_v2::ZwpTabletToolV2, ()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (zwp_tablet_pad_v2::ZwpTabletPadV2, ())
    ]);
}

delegate_noop!(TabletProbe: ignore zwp_tablet_v2::ZwpTabletV2);
delegate_noop!(TabletProbe: ignore zwp_tablet_tool_v2::ZwpTabletToolV2);
delegate_noop!(TabletProbe: ignore zwp_tablet_pad_v2::ZwpTabletPadV2);

#[derive(Default)]
struct InputMethodProbe {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<zwp_input_method_manager_v2::ZwpInputMethodManagerV2>,
    input_method: Option<zwp_input_method_v2::ZwpInputMethodV2>,
    active: bool,
    done_serial: u32,
    commit_sent: bool,
    unavailable: bool,
    saw_text_input_manager: bool,
    saw_surrounding_text: bool,
    saw_text_change_cause: bool,
    saw_content_type: bool,
    ready_printed: bool,
}

fn probe_input_method() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = InputMethodProbe::default();
    for _ in 0..8 {
        event_queue.roundtrip(&mut state)?;
        state.initialize(&queue);
        if state.input_method.is_some() && !state.ready_printed {
            ensure!(
                state.saw_text_input_manager,
                "authorized input method did not see the text-input manager"
            );
            println!("input-method-ready");
            std::io::stdout().flush()?;
            state.ready_printed = true;
        }
        if state.ready_printed {
            break;
        }
    }
    ensure!(state.ready_printed, "input method could not bind its seat");
    loop {
        event_queue.blocking_dispatch(&mut state)?;
        ensure!(
            !state.unavailable,
            "input method was unexpectedly unavailable"
        );
        if state.commit_sent {
            connection.flush()?;
            event_queue.roundtrip(&mut state)?;
            println!("input-method-commit-ok");
            return Ok(());
        }
    }
}

impl InputMethodProbe {
    fn initialize(&mut self, queue: &QueueHandle<Self>) {
        if self.input_method.is_none()
            && let (Some(manager), Some(seat)) = (&self.manager, &self.seat)
        {
            self.input_method = Some(manager.get_input_method(seat, queue, ()));
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for InputMethodProbe {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_seat" => state.seat = Some(registry.bind(name, version.min(9), queue, ())),
            "zwp_input_method_manager_v2" => {
                state.manager = Some(registry.bind(name, version.min(1), queue, ()))
            }
            "zwp_text_input_manager_v3" => state.saw_text_input_manager = true,
            _ => {}
        }
        state.initialize(queue);
    }
}

delegate_noop!(InputMethodProbe: ignore wl_seat::WlSeat);
delegate_noop!(InputMethodProbe: ignore zwp_input_method_manager_v2::ZwpInputMethodManagerV2);

impl Dispatch<zwp_input_method_v2::ZwpInputMethodV2, ()> for InputMethodProbe {
    fn event(
        state: &mut Self,
        input_method: &zwp_input_method_v2::ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.active = true,
            zwp_input_method_v2::Event::Deactivate => state.active = false,
            zwp_input_method_v2::Event::SurroundingText {
                text,
                cursor,
                anchor,
            } => {
                state.saw_surrounding_text = text == "hello" && cursor == 5 && anchor == 5;
            }
            zwp_input_method_v2::Event::TextChangeCause { .. } => {
                state.saw_text_change_cause = true;
            }
            zwp_input_method_v2::Event::ContentType { .. } => {
                state.saw_content_type = true;
            }
            zwp_input_method_v2::Event::Done => {
                if state.active
                    && state.saw_surrounding_text
                    && state.saw_text_change_cause
                    && state.saw_content_type
                    && !state.commit_sent
                {
                    input_method.commit_string("nobox-ime".to_owned());
                    input_method.commit(state.done_serial);
                    state.commit_sent = true;
                }
                state.done_serial = state.done_serial.wrapping_add(1);
            }
            zwp_input_method_v2::Event::Unavailable => state.unavailable = true,
            _ => {}
        }
    }
}

delegate_noop!(InputMethodProbe: ignore zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2);
delegate_noop!(InputMethodProbe: ignore zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2);

fn poll_selection_pipe(
    reader: &mut Option<UnixStream>,
    payload: &mut Vec<u8>,
    expected: &[u8],
) -> Result<bool> {
    let Some(stream) = reader else {
        return Ok(false);
    };
    let mut buffer = [0_u8; 256];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => {
                *reader = None;
                return Ok(payload == expected);
            }
            Ok(length) => payload.extend_from_slice(&buffer[..length]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
}

#[derive(Clone, Copy)]
enum SelectionLimit {
    Sources,
    Devices,
    MimeCount,
    MimeSize,
}

#[derive(Default)]
struct SelectionLimitProbe {
    seat: Option<wl_seat::WlSeat>,
    data_manager: Option<wl_data_device_manager::WlDataDeviceManager>,
    primary_manager:
        Option<zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1>,
    data_sources: Vec<wl_data_source::WlDataSource>,
    primary_sources: Vec<zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1>,
    data_devices: Vec<wl_data_device::WlDataDevice>,
    primary_devices: Vec<zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1>,
}

fn probe_selection_resource_limit(limit: SelectionLimit) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = SelectionLimitProbe::default();
    event_queue.roundtrip(&mut state)?;
    let data_manager = state
        .data_manager
        .clone()
        .context("wl_data_device_manager was not advertised")?;
    let primary_manager = state
        .primary_manager
        .clone()
        .context("primary selection manager was not advertised")?;

    match limit {
        SelectionLimit::Sources => {
            for _ in 0..32 {
                state
                    .data_sources
                    .push(data_manager.create_data_source(&queue, ()));
            }
            for _ in 0..33 {
                state
                    .primary_sources
                    .push(primary_manager.create_source(&queue, ()));
            }
        }
        SelectionLimit::Devices => {
            let seat = state.seat.clone().context("wl_seat was not advertised")?;
            for _ in 0..8 {
                state
                    .data_devices
                    .push(data_manager.get_data_device(&seat, &queue, ()));
            }
            for _ in 0..9 {
                state
                    .primary_devices
                    .push(primary_manager.get_device(&seat, &queue, ()));
            }
        }
        SelectionLimit::MimeCount => {
            let source = data_manager.create_data_source(&queue, ());
            for index in 0..33 {
                source.offer(format!("application/x-nobox-{index}"));
            }
            state.data_sources.push(source);
        }
        SelectionLimit::MimeSize => {
            let source = primary_manager.create_source(&queue, ());
            source.offer("x".repeat(257));
            state.primary_sources.push(source);
        }
    }

    ensure!(
        event_queue.roundtrip(&mut state).is_err(),
        "selection resource limit did not disconnect the hostile client"
    );
    let name = match limit {
        SelectionLimit::Sources => "sources",
        SelectionLimit::Devices => "devices",
        SelectionLimit::MimeCount => "mime-count",
        SelectionLimit::MimeSize => "mime-size",
    };
    println!("selection-limit-ok {name}");
    Ok(())
}

impl Dispatch<wl_registry::WlRegistry, ()> for SelectionLimitProbe {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_seat" => {
                state.seat = Some(registry.bind(name, version.min(9), queue, ()));
            }
            "wl_data_device_manager" => {
                state.data_manager = Some(registry.bind(name, version.min(3), queue, ()));
            }
            "zwp_primary_selection_device_manager_v1" => {
                state.primary_manager = Some(registry.bind(name, version.min(1), queue, ()));
            }
            _ => {}
        }
    }
}

delegate_noop!(SelectionLimitProbe: ignore wl_seat::WlSeat);
delegate_noop!(SelectionLimitProbe: ignore wl_data_device_manager::WlDataDeviceManager);
delegate_noop!(SelectionLimitProbe: ignore wl_data_device::WlDataDevice);
delegate_noop!(SelectionLimitProbe: ignore wl_data_offer::WlDataOffer);
delegate_noop!(SelectionLimitProbe: ignore wl_data_source::WlDataSource);
delegate_noop!(SelectionLimitProbe: ignore zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1);
delegate_noop!(SelectionLimitProbe: ignore zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1);
delegate_noop!(SelectionLimitProbe: ignore zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1);
delegate_noop!(SelectionLimitProbe: ignore zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1);

#[derive(Default)]
struct SurfaceLimitProbe {
    compositor: Option<wl_compositor::WlCompositor>,
    surfaces: Vec<wl_surface::WlSurface>,
}

fn probe_surface_limit() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = SurfaceLimitProbe::default();
    event_queue.roundtrip(&mut state)?;
    let compositor = state
        .compositor
        .clone()
        .context("wl_compositor global was not advertised")?;
    for _ in 0..=256 {
        state.surfaces.push(compositor.create_surface(&queue, ()));
    }
    ensure!(
        event_queue.roundtrip(&mut state).is_err(),
        "client exceeding the surface limit was not disconnected"
    );
    println!("surface-limit-ok");
    Ok(())
}

impl Dispatch<wl_registry::WlRegistry, ()> for SurfaceLimitProbe {
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
            && interface == "wl_compositor"
        {
            state.compositor = Some(registry.bind(name, version.min(5), queue, ()));
        }
    }
}

delegate_noop!(SurfaceLimitProbe: ignore wl_compositor::WlCompositor);
delegate_noop!(SurfaceLimitProbe: ignore wl_surface::WlSurface);

#[derive(Clone, Copy)]
enum CoreLimit {
    Callbacks,
    ShmPools,
    ShmBuffers,
    ShmSize,
    ShmDimension,
    XdgPositioners,
    XdgPopups,
    PendingConfigures,
    WlrForeignManagers,
}

#[derive(Default)]
struct CoreLimitProbe {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    shell: Option<xdg_wm_base::XdgWmBase>,
    surfaces: Vec<wl_surface::WlSurface>,
    callbacks: Vec<wl_callback::WlCallback>,
    pools: Vec<wl_shm_pool::WlShmPool>,
    buffers: Vec<wl_buffer::WlBuffer>,
    positioners: Vec<xdg_positioner::XdgPositioner>,
    xdg_surfaces: Vec<xdg_surface::XdgSurface>,
    toplevels: Vec<xdg_toplevel::XdgToplevel>,
    popups: Vec<xdg_popup::XdgPopup>,
    backing_file: Option<File>,
    limit: Option<CoreLimit>,
    wlr_foreign_managers: Vec<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1>,
}

fn probe_core_resource_limit(limit: CoreLimit) -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = CoreLimitProbe {
        limit: Some(limit),
        ..CoreLimitProbe::default()
    };
    event_queue.roundtrip(&mut state)?;

    match limit {
        CoreLimit::Callbacks => {
            let compositor = state
                .compositor
                .clone()
                .context("wl_compositor global was not advertised")?;
            let surface = compositor.create_surface(&queue, ());
            for _ in 0..=1024 {
                state.callbacks.push(surface.frame(&queue, ()));
            }
            state.surfaces.push(surface);
        }
        CoreLimit::ShmPools
        | CoreLimit::ShmBuffers
        | CoreLimit::ShmSize
        | CoreLimit::ShmDimension => {
            let shm = state
                .shm
                .clone()
                .context("wl_shm global was not advertised")?;
            let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .context("XDG_RUNTIME_DIR is unset")?;
            let path = runtime.join(format!("nobox-resource-probe-{}", std::process::id()));
            let file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)?;
            std::fs::remove_file(path)?;
            file.set_len(4)?;
            match limit {
                CoreLimit::ShmPools => {
                    for _ in 0..=64 {
                        state
                            .pools
                            .push(shm.create_pool(file.as_fd(), 4, &queue, ()));
                    }
                }
                CoreLimit::ShmBuffers => {
                    let pool = shm.create_pool(file.as_fd(), 4, &queue, ());
                    for _ in 0..=4096 {
                        state.buffers.push(pool.create_buffer(
                            0,
                            1,
                            1,
                            4,
                            wl_shm::Format::Argb8888,
                            &queue,
                            (),
                        ));
                    }
                    state.pools.push(pool);
                }
                CoreLimit::ShmSize => {
                    state.pools.push(shm.create_pool(
                        file.as_fd(),
                        64 * 1024 * 1024 + 1,
                        &queue,
                        (),
                    ));
                }
                CoreLimit::ShmDimension => {
                    let pool = shm.create_pool(file.as_fd(), 4, &queue, ());
                    state.buffers.push(pool.create_buffer(
                        0,
                        16_385,
                        1,
                        4,
                        wl_shm::Format::Argb8888,
                        &queue,
                        (),
                    ));
                    state.pools.push(pool);
                }
                CoreLimit::Callbacks
                | CoreLimit::XdgPositioners
                | CoreLimit::XdgPopups
                | CoreLimit::PendingConfigures
                | CoreLimit::WlrForeignManagers => unreachable!(),
            }
            state.backing_file = Some(file);
        }
        CoreLimit::XdgPositioners => {
            let shell = state
                .shell
                .clone()
                .context("xdg_wm_base global was not advertised")?;
            for _ in 0..=256 {
                state.positioners.push(shell.create_positioner(&queue, ()));
            }
        }
        CoreLimit::XdgPopups | CoreLimit::PendingConfigures => {
            let compositor = state
                .compositor
                .clone()
                .context("wl_compositor global was not advertised")?;
            let shell = state
                .shell
                .clone()
                .context("xdg_wm_base global was not advertised")?;
            let count = if matches!(limit, CoreLimit::XdgPopups) {
                129
            } else {
                1
            };
            let parent_surface = compositor.create_surface(&queue, ());
            let parent_xdg_surface = shell.get_xdg_surface(&parent_surface, &queue, ());
            let parent_toplevel = parent_xdg_surface.get_toplevel(&queue, ());
            state.surfaces.push(parent_surface);
            state.xdg_surfaces.push(parent_xdg_surface.clone());
            state.toplevels.push(parent_toplevel);
            for _ in 0..count {
                let surface = compositor.create_surface(&queue, ());
                let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
                let positioner = shell.create_positioner(&queue, ());
                positioner.set_size(1, 1);
                positioner.set_anchor_rect(0, 0, 1, 1);
                if matches!(limit, CoreLimit::PendingConfigures) {
                    positioner.set_reactive();
                }
                let popup =
                    xdg_surface.get_popup(Some(&parent_xdg_surface), &positioner, &queue, ());
                surface.commit();
                state.surfaces.push(surface);
                state.xdg_surfaces.push(xdg_surface);
                state.positioners.push(positioner);
                state.popups.push(popup);
            }
            if matches!(limit, CoreLimit::PendingConfigures) {
                event_queue.roundtrip(&mut state)?;
                let popup = state.popups[0].clone();
                let positioner = state.positioners[0].clone();
                for token in 0..=64 {
                    popup.reposition(&positioner, token);
                }
            }
        }
        CoreLimit::WlrForeignManagers => {}
    }

    ensure!(
        event_queue.roundtrip(&mut state).is_err(),
        "core resource limit did not disconnect the hostile client"
    );
    println!("core-resource-limit-ok");
    Ok(())
}

fn probe_core_resource_churn() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = CoreLimitProbe::default();
    event_queue.roundtrip(&mut state)?;

    let shm = state
        .shm
        .clone()
        .context("wl_shm global was not advertised")?;
    let shell = state
        .shell
        .clone()
        .context("xdg_wm_base global was not advertised")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .context("XDG_RUNTIME_DIR is unset")?;
    let path = runtime.join(format!("nobox-resource-churn-{}", std::process::id()));
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)?;
    std::fs::remove_file(path)?;
    file.set_len(4)?;

    for _ in 0..16 {
        for _ in 0..32 {
            let pool = shm.create_pool(file.as_fd(), 4, &queue, ());
            let buffer = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Argb8888, &queue, ());
            let positioner = shell.create_positioner(&queue, ());
            buffer.destroy();
            pool.destroy();
            positioner.destroy();
        }
        event_queue.roundtrip(&mut state)?;
    }
    println!("core-resource-churn-ok");
    Ok(())
}

impl Dispatch<wl_registry::WlRegistry, ()> for CoreLimitProbe {
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
                    state.compositor = Some(registry.bind(name, version.min(5), queue, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, version.min(2), queue, ())),
                "xdg_wm_base" => state.shell = Some(registry.bind(name, version.min(6), queue, ())),
                "zwlr_foreign_toplevel_manager_v1"
                    if matches!(state.limit, Some(CoreLimit::WlrForeignManagers)) =>
                {
                    for _ in 0..=16 {
                        state.wlr_foreign_managers.push(registry.bind(
                            name,
                            version.min(3),
                            queue,
                            (),
                        ));
                    }
                }
                _ => {}
            }
        }
    }
}

delegate_noop!(CoreLimitProbe: ignore wl_compositor::WlCompositor);
delegate_noop!(CoreLimitProbe: ignore wl_surface::WlSurface);
delegate_noop!(CoreLimitProbe: ignore wl_callback::WlCallback);
delegate_noop!(CoreLimitProbe: ignore wl_shm::WlShm);
delegate_noop!(CoreLimitProbe: ignore wl_shm_pool::WlShmPool);
delegate_noop!(CoreLimitProbe: ignore wl_buffer::WlBuffer);
delegate_noop!(CoreLimitProbe: ignore xdg_positioner::XdgPositioner);
delegate_noop!(CoreLimitProbe: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(CoreLimitProbe: ignore zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1);
delegate_noop!(CoreLimitProbe: ignore xdg_popup::XdgPopup);

impl Dispatch<xdg_surface::XdgSurface, ()> for CoreLimitProbe {
    fn event(
        _state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for CoreLimitProbe {
    fn event(
        _state: &mut Self,
        shell: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            shell.pong(serial);
        }
    }
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

fn probe_agent_hold() -> Result<()> {
    let (_connection, mut event_queue, mut state) =
        connected_shell_probe_named(Some("nobox Wayland agent visible"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while std::time::Instant::now() < deadline && !state.close_received {
        event_queue.roundtrip(&mut state)?;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    println!("agent-hold-ok");
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
    // The default compositor binding is Super-q. Xvfb/Xephyr's standard XKB
    // map exposes the left Super key as 133 and q as 24.
    inject_parent_input(&[
        (KEY_PRESS_EVENT, 133, 0, 0),
        (KEY_PRESS_EVENT, 24, 0, 0),
        (KEY_RELEASE_EVENT, 24, 0, 0),
        (KEY_RELEASE_EVENT, 133, 0, 0),
    ])?;
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.close_received {
            println!("close-ok");
            return Ok(());
        }
    }
    anyhow::bail!("focused client did not receive xdg_toplevel.close")
}

fn probe_decoration_close() -> Result<()> {
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
    ensure!(state.configured, "decoration-close probe did not map");
    // The probe's fixed-size surface is centered. These normalized parent
    // coordinates land in the close button generated from the default 24px titlebar.
    inject_parent_input(&[
        (MOTION_NOTIFY_EVENT, 0, 397, 121),
        (BUTTON_PRESS_EVENT, 1, 397, 121),
        (BUTTON_RELEASE_EVENT, 1, 397, 121),
    ])?;
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.close_received {
            println!("decoration-close-ok");
            return Ok(());
        }
    }
    anyhow::bail!("decorated close hit target did not send xdg_toplevel.close")
}

fn probe_keyboard_resize() -> Result<()> {
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
    ensure!(state.configured, "keyboard-resize probe did not map");

    // The test config binds Super-r to the protocol-neutral Resize action.
    inject_parent_input(&[
        (KEY_PRESS_EVENT, 133, 0, 0),
        (KEY_PRESS_EVENT, 27, 0, 0),
        (KEY_RELEASE_EVENT, 27, 0, 0),
        (KEY_RELEASE_EVENT, 133, 0, 0),
    ])?;
    for _ in 0..3 {
        event_queue.roundtrip(&mut state)?;
    }
    let initial = state
        .last_configure_size
        .context("keyboard resize did not enter xdg resizing state")?;

    // The first arrow selects the controlled edge; the second moves it.
    inject_parent_input(&[
        (KEY_PRESS_EVENT, 114, 0, 0),
        (KEY_RELEASE_EVENT, 114, 0, 0),
        (KEY_PRESS_EVENT, 114, 0, 0),
        (KEY_RELEASE_EVENT, 114, 0, 0),
        (KEY_PRESS_EVENT, 36, 0, 0),
        (KEY_RELEASE_EVENT, 36, 0, 0),
    ])?;
    for _ in 0..5 {
        event_queue.roundtrip(&mut state)?;
    }
    let resized = state
        .last_configure_size
        .context("keyboard resize produced no final configure")?;
    ensure!(
        resized.0 > initial.0 && resized.1 == initial.1,
        "keyboard resize changed {:?} to {:?}; expected horizontal growth",
        initial,
        resized
    );
    println!("keyboard-resize-ok initial={initial:?} resized={resized:?}");
    Ok(())
}

fn probe_mouse_resize() -> Result<()> {
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
    ensure!(state.configured, "mouse-resize probe did not map");
    inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 300, 180)])?;
    for _ in 0..2 {
        event_queue.roundtrip(&mut state)?;
    }
    inject_parent_input(&[
        (KEY_PRESS_EVENT, 133, 0, 0),
        (BUTTON_PRESS_EVENT, 1, 300, 180),
    ])?;
    for _ in 0..2 {
        event_queue.roundtrip(&mut state)?;
    }
    inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 250, 180)])?;
    for _ in 0..5 {
        event_queue.roundtrip(&mut state)?;
    }
    let initial = state
        .last_configure_size
        .context("configured mouse drag produced no initial resize configure")?;
    inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, 300, 180)])?;
    for _ in 0..5 {
        event_queue.roundtrip(&mut state)?;
    }
    inject_parent_input(&[
        (BUTTON_RELEASE_EVENT, 1, 300, 180),
        (KEY_RELEASE_EVENT, 133, 0, 0),
    ])?;
    for _ in 0..3 {
        event_queue.roundtrip(&mut state)?;
    }
    let resized = state
        .last_configure_size
        .context("configured mouse drag produced no resize configure")?;
    ensure!(
        resized.0 > initial.0 && resized.1 == initial.1,
        "mouse resize changed {initial:?} to {resized:?}; expected horizontal growth"
    );
    if state.workspaces.len() > 1 {
        inject_parent_input(&[
            (MOTION_NOTIFY_EVENT, 0, 620, 340),
            (BUTTON_PRESS_EVENT, 4, 620, 340),
        ])?;
        for _ in 0..4 {
            event_queue.roundtrip(&mut state)?;
        }
        ensure!(
            state.workspaces[1].active,
            "configured root wheel binding did not switch workspace"
        );
        inject_parent_input(&[(BUTTON_RELEASE_EVENT, 4, 620, 340)])?;
    }
    println!("mouse-resize-ok initial={initial:?} resized={resized:?}");
    Ok(())
}

fn connected_shell_probe() -> Result<(
    Connection,
    wayland_client::EventQueue<ShellProbe>,
    ShellProbe,
)> {
    connected_shell_probe_named(None)
}

fn connected_shell_probe_named(
    title: Option<&str>,
) -> Result<(
    Connection,
    wayland_client::EventQueue<ShellProbe>,
    ShellProbe,
)> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = ShellProbe {
        respond_to_ping: true,
        requested_title: title.map(str::to_owned),
        ..ShellProbe::default()
    };
    let mut environment_activation = std::env::var("XDG_ACTIVATION_TOKEN")
        .ok()
        .filter(|token| !token.is_empty());
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if let Some(token) = environment_activation.take() {
            if let (Some(activation), Some(surface)) = (&state.activation, &state.surface) {
                activation.activate(token, surface);
            } else {
                environment_activation = Some(token);
            }
        }
        if state.configured && state.frame_callbacks > 0 {
            break;
        }
    }
    ensure!(state.configured, "focus-cycle client did not map");
    Ok((connection, event_queue, state))
}

fn dispatch_shell_pair(
    queue_a: &mut wayland_client::EventQueue<ShellProbe>,
    state_a: &mut ShellProbe,
    queue_b: &mut wayland_client::EventQueue<ShellProbe>,
    state_b: &mut ShellProbe,
) -> Result<()> {
    for _ in 0..2 {
        queue_a.roundtrip(state_a)?;
        queue_b.roundtrip(state_b)?;
    }
    Ok(())
}

fn probe_focus_cycle() -> Result<()> {
    const ALT: u8 = 64;
    const SHIFT: u8 = 50;
    const TAB: u8 = 23;
    const ESCAPE: u8 = 9;
    const A: u8 = 38;
    const WAYLAND_A: u32 = 30;

    let (_connection_a, mut queue_a, mut state_a) = connected_shell_probe()?;
    let (_connection_b, mut queue_b, mut state_b) = connected_shell_probe()?;

    // The second client maps focused. Alt-Tab previews the first without
    // raising it, and the held session paints the compositor overlay.
    inject_parent_input(&[
        (KEY_PRESS_EVENT, ALT, 0, 0),
        (KEY_PRESS_EVENT, TAB, 0, 0),
        (KEY_RELEASE_EVENT, TAB, 0, 0),
    ])?;
    dispatch_shell_pair(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b)?;
    std::thread::sleep(std::time::Duration::from_millis(50));
    let center = parent_center_pixel()?;
    ensure!(
        center[..3] != [0x30, 0x80, 0xd0],
        "focus switcher did not cover the client at the output center: {center:?}"
    );
    inject_parent_input(&[
        (KEY_RELEASE_EVENT, ALT, 0, 0),
        (KEY_PRESS_EVENT, A, 0, 0),
        (KEY_RELEASE_EVENT, A, 0, 0),
    ])?;
    dispatch_shell_pair(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b)?;
    ensure!(
        state_a
            .keycodes
            .iter()
            .filter(|key| **key == WAYLAND_A)
            .count()
            == 2,
        "Alt-Tab did not commit focus to the previous client"
    );

    // Shift is only the direction selector: releasing it must not end the
    // session before the primary Alt modifier is released.
    inject_parent_input(&[
        (KEY_PRESS_EVENT, ALT, 0, 0),
        (KEY_PRESS_EVENT, SHIFT, 0, 0),
        (KEY_PRESS_EVENT, TAB, 0, 0),
        (KEY_RELEASE_EVENT, TAB, 0, 0),
        (KEY_RELEASE_EVENT, SHIFT, 0, 0),
        (KEY_RELEASE_EVENT, ALT, 0, 0),
        (KEY_PRESS_EVENT, A, 0, 0),
        (KEY_RELEASE_EVENT, A, 0, 0),
    ])?;
    dispatch_shell_pair(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b)?;
    ensure!(
        state_b
            .keycodes
            .iter()
            .filter(|key| **key == WAYLAND_A)
            .count()
            == 2,
        "Alt-Shift-Tab did not commit reverse focus cycling"
    );

    // Escape restores the client focused before the held cycle began.
    inject_parent_input(&[
        (KEY_PRESS_EVENT, ALT, 0, 0),
        (KEY_PRESS_EVENT, TAB, 0, 0),
        (KEY_RELEASE_EVENT, TAB, 0, 0),
        (KEY_PRESS_EVENT, ESCAPE, 0, 0),
        (KEY_RELEASE_EVENT, ESCAPE, 0, 0),
        (KEY_RELEASE_EVENT, ALT, 0, 0),
        (KEY_PRESS_EVENT, A, 0, 0),
        (KEY_RELEASE_EVENT, A, 0, 0),
    ])?;
    dispatch_shell_pair(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b)?;
    ensure!(
        state_b
            .keycodes
            .iter()
            .filter(|key| **key == WAYLAND_A)
            .count()
            == 4,
        "Escape did not restore focus during Alt-Tab"
    );
    println!("focus-cycle-ok center={center:?}");
    Ok(())
}

fn probe_directional_cycle() -> Result<()> {
    const ALT: u8 = 64;
    const H: u8 = 43;
    const J: u8 = 44;
    const K: u8 = 45;
    const L: u8 = 46;
    const ESCAPE: u8 = 9;
    const A: u8 = 38;
    const WAYLAND_A: u32 = 30;

    let (_connection_a, mut queue_a, mut state_a) = connected_shell_probe()?;
    let (_connection_b, mut queue_b, mut state_b) = connected_shell_probe()?;

    // Smart placement may choose either axis. Find the actual direction from
    // the second native client to the first and prove release-to-commit.
    let mut reverse = None;
    for (direction, opposite) in [(H, L), (L, H), (J, K), (K, J)] {
        let before = state_a
            .keycodes
            .iter()
            .filter(|key| **key == WAYLAND_A)
            .count();
        inject_parent_input(&[
            (KEY_PRESS_EVENT, ALT, 0, 0),
            (KEY_PRESS_EVENT, direction, 0, 0),
            (KEY_RELEASE_EVENT, direction, 0, 0),
            (KEY_RELEASE_EVENT, ALT, 0, 0),
            (KEY_PRESS_EVENT, A, 0, 0),
            (KEY_RELEASE_EVENT, A, 0, 0),
        ])?;
        dispatch_shell_pair(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b)?;
        let after = state_a
            .keycodes
            .iter()
            .filter(|key| **key == WAYLAND_A)
            .count();
        if after == before.saturating_add(2) {
            reverse = Some(opposite);
            break;
        }
    }
    let reverse = reverse.context("no spatial direction selected the peer client")?;

    // The reverse direction previews the peer and paints the same compositor
    // overlay as linear cycling, but Escape restores the original focus.
    inject_parent_input(&[
        (KEY_PRESS_EVENT, ALT, 0, 0),
        (KEY_PRESS_EVENT, reverse, 0, 0),
        (KEY_RELEASE_EVENT, reverse, 0, 0),
    ])?;
    dispatch_shell_pair(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b)?;
    std::thread::sleep(std::time::Duration::from_millis(50));
    let center = parent_center_pixel()?;
    ensure!(
        center[..3] != [0x30, 0x80, 0xd0],
        "directional switcher did not paint its overlay: {center:?}"
    );
    inject_parent_input(&[
        (KEY_PRESS_EVENT, ESCAPE, 0, 0),
        (KEY_RELEASE_EVENT, ESCAPE, 0, 0),
        (KEY_RELEASE_EVENT, ALT, 0, 0),
        (KEY_PRESS_EVENT, A, 0, 0),
        (KEY_RELEASE_EVENT, A, 0, 0),
    ])?;
    dispatch_shell_pair(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b)?;
    ensure!(
        state_a
            .keycodes
            .iter()
            .filter(|key| **key == WAYLAND_A)
            .count()
            == 4,
        "Escape did not restore focus during a directional cycle"
    );
    println!("directional-cycle-ok center={center:?}");
    Ok(())
}

fn probe_attention() -> Result<()> {
    let (_connection_a, mut queue_a, mut state_a) = connected_shell_probe()?;
    let (_connection_b, mut queue_b, mut state_b) = connected_shell_probe()?;
    let before = parent_pixels()?;
    let activation = state_a
        .activation
        .clone()
        .context("xdg activation was not advertised")?;
    let surface = state_a
        .surface
        .clone()
        .context("attention probe has no surface")?;
    let token = activation.get_activation_token(&queue_a.handle(), ());
    token.set_surface(&surface);
    token.set_app_id("org.nobox.attention-probe".to_owned());
    token.commit();
    state_a.activation_done = false;
    state_a.activation_token = Some(token);
    for _ in 0..2 {
        dispatch_shell_pair(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b)?;
    }
    ensure!(
        state_a.activation_done,
        "invalid activation token was not completed"
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
    let urgent = parent_pixels()?;
    ensure!(
        urgent != before,
        "invalid activation did not produce visible attention feedback"
    );

    inject_parent_input(&[
        (KEY_PRESS_EVENT, 64, 0, 0),
        (KEY_PRESS_EVENT, 23, 0, 0),
        (KEY_RELEASE_EVENT, 23, 0, 0),
        (KEY_RELEASE_EVENT, 64, 0, 0),
    ])?;
    dispatch_shell_pair(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b)?;
    std::thread::sleep(std::time::Duration::from_millis(50));
    let focused = parent_pixels()?;
    ensure!(
        focused != urgent,
        "focusing an urgent client did not clear its attention rendering"
    );
    println!(
        "attention-ok changed_pixels={}",
        pixel_differences(&before, &urgent)
    );
    Ok(())
}

fn probe_follow_mouse() -> Result<()> {
    let (_connection_a, mut queue_a, mut state_a) =
        connected_shell_probe_named(Some("nobox follow A"))?;
    let (_connection_b, mut queue_b, mut state_b) =
        connected_shell_probe_named(Some("nobox follow B"))?;

    let first =
        focus_client_by_pointer(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b, true)
            .context("pointer entry did not focus the first native client")?;
    let second = focus_client_by_pointer(
        &mut queue_a,
        &mut state_a,
        &mut queue_b,
        &mut state_b,
        false,
    )
    .context("pointer entry did not focus the second native client")?;
    println!("follow-mouse-ok first={first:?} second={second:?}");
    Ok(())
}

fn focus_client_by_pointer(
    queue_a: &mut wayland_client::EventQueue<ShellProbe>,
    state_a: &mut ShellProbe,
    queue_b: &mut wayland_client::EventQueue<ShellProbe>,
    state_b: &mut ShellProbe,
    first: bool,
) -> Result<Option<(i16, i16)>> {
    const A: u8 = 38;
    const WAYLAND_A: u32 = 30;
    let preferred = [(160, 130), (480, 250)];
    let grid = (10..360)
        .step_by(30)
        .flat_map(|y| (10..640).step_by(30).map(move |x| (x, y)));
    for (x, y) in preferred.into_iter().chain(grid) {
        let before = if first {
            state_a.keycodes.iter()
        } else {
            state_b.keycodes.iter()
        }
        .filter(|key| **key == WAYLAND_A)
        .count();
        inject_parent_input(&[(MOTION_NOTIFY_EVENT, 0, x, y)])?;
        dispatch_shell_pair(queue_a, state_a, queue_b, state_b)?;
        inject_parent_input(&[(KEY_PRESS_EVENT, A, 0, 0), (KEY_RELEASE_EVENT, A, 0, 0)])?;
        dispatch_shell_pair(queue_a, state_a, queue_b, state_b)?;
        let after = if first {
            state_a.keycodes.iter()
        } else {
            state_b.keycodes.iter()
        }
        .filter(|key| **key == WAYLAND_A)
        .count();
        if after == before.saturating_add(2) {
            return Ok(Some((x, y)));
        }
    }
    Ok(None)
}

fn probe_activation_permissive() -> Result<()> {
    const A: u8 = 38;
    const WAYLAND_A: u32 = 30;
    let (_connection_a, mut queue_a, mut state_a) = connected_shell_probe()?;
    let (_connection_b, mut queue_b, mut state_b) = connected_shell_probe()?;
    let activation = state_a
        .activation
        .clone()
        .context("xdg activation was not advertised")?;
    let surface = state_a
        .surface
        .clone()
        .context("activation probe has no surface")?;
    let token = activation.get_activation_token(&queue_a.handle(), ());
    token.set_surface(&surface);
    token.set_app_id("org.nobox.activation-permissive".to_owned());
    token.commit();
    state_a.activation_done = false;
    state_a.activation_token = Some(token);
    for _ in 0..2 {
        dispatch_shell_pair(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b)?;
    }
    ensure!(
        state_a.activation_done,
        "activation token was not completed"
    );
    inject_parent_input(&[(KEY_PRESS_EVENT, A, 0, 0), (KEY_RELEASE_EVENT, A, 0, 0)])?;
    dispatch_shell_pair(&mut queue_a, &mut state_a, &mut queue_b, &mut state_b)?;
    ensure!(
        state_a
            .keycodes
            .iter()
            .filter(|key| **key == WAYLAND_A)
            .count()
            == 2,
        "prevent_focus_stealing=false did not accept a fresh unproven activation"
    );
    println!("activation-permissive-ok");
    Ok(())
}

fn probe_menu() -> Result<()> {
    let (_connection, mut event_queue, mut state) = connected_shell_probe()?;
    // Alt-Space opens the focused-client menu. End selects its final Close
    // operation and Return activates it through the normal action executor.
    inject_parent_input(&[
        (KEY_PRESS_EVENT, 64, 0, 0),
        (KEY_PRESS_EVENT, 65, 0, 0),
        (KEY_RELEASE_EVENT, 65, 0, 0),
        (KEY_RELEASE_EVENT, 64, 0, 0),
    ])?;
    for _ in 0..2 {
        event_queue.roundtrip(&mut state)?;
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    let center = parent_center_pixel()?;
    ensure!(
        center[..3] != [0x30, 0x80, 0xd0],
        "client menu did not cover the client at the output center: {center:?}"
    );
    inject_parent_input(&[
        (KEY_PRESS_EVENT, 115, 0, 0),
        (KEY_RELEASE_EVENT, 115, 0, 0),
        (KEY_PRESS_EVENT, 36, 0, 0),
        (KEY_RELEASE_EVENT, 36, 0, 0),
    ])?;
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.close_received {
            println!("menu-ok center={center:?}");
            return Ok(());
        }
    }
    anyhow::bail!("client menu did not dispatch its selected Close action")
}

fn open_super_menu(keycode: u8) -> Result<()> {
    inject_parent_input(&[
        (KEY_PRESS_EVENT, 133, 0, 0),
        (KEY_PRESS_EVENT, keycode, 0, 0),
        (KEY_RELEASE_EVENT, keycode, 0, 0),
        (KEY_RELEASE_EVENT, 133, 0, 0),
    ])
}

fn probe_command_menu() -> Result<()> {
    let (_connection, mut event_queue, mut state) = connected_shell_probe()?;
    // The configured Super-m menu is generated by a bounded shell command.
    // End enters the overflow continuation and another End selects Close.
    open_super_menu(58)?;
    for _ in 0..2 {
        event_queue.roundtrip(&mut state)?;
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    let center = parent_center_pixel()?;
    ensure!(
        center[..3] != [0x30, 0x80, 0xd0],
        "command menu did not render: {center:?}"
    );
    inject_parent_input(&[
        (KEY_PRESS_EVENT, 115, 0, 0),
        (KEY_RELEASE_EVENT, 115, 0, 0),
        (KEY_PRESS_EVENT, 36, 0, 0),
        (KEY_RELEASE_EVENT, 36, 0, 0),
        (KEY_PRESS_EVENT, 115, 0, 0),
        (KEY_RELEASE_EVENT, 115, 0, 0),
        (KEY_PRESS_EVENT, 36, 0, 0),
        (KEY_RELEASE_EVENT, 36, 0, 0),
    ])?;
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.close_received {
            println!("command-menu-ok center={center:?}");
            return Ok(());
        }
    }
    anyhow::bail!("command menu did not dispatch its generated Close action")
}

fn probe_application_menu() -> Result<()> {
    let (_connection, mut event_queue, mut state) = connected_shell_probe()?;
    // Super-a opens the XDG catalog; the first Return enters its only category
    // and the second safely launches the only desktop entry.
    open_super_menu(38)?;
    for _ in 0..2 {
        event_queue.roundtrip(&mut state)?;
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    let center = parent_center_pixel()?;
    ensure!(
        center[..3] != [0x30, 0x80, 0xd0],
        "application menu did not render: {center:?}"
    );
    inject_parent_input(&[
        (KEY_PRESS_EVENT, 36, 0, 0),
        (KEY_RELEASE_EVENT, 36, 0, 0),
        (KEY_PRESS_EVENT, 36, 0, 0),
        (KEY_RELEASE_EVENT, 36, 0, 0),
    ])?;
    for _ in 0..2 {
        event_queue.roundtrip(&mut state)?;
    }
    println!("application-menu-ok center={center:?}");
    Ok(())
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
        (MOTION_NOTIFY_EVENT, 0, 300, 180),
        (BUTTON_PRESS_EVENT, 1, 300, 180),
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

fn probe_layer_shell() -> Result<()> {
    let connection = Connection::connect_to_env()?;
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let mut state = LayerProbe::default();
    event_queue.roundtrip(&mut state)?;
    let compositor = state
        .compositor
        .clone()
        .context("wl_compositor was not advertised")?;
    let shm = state.shm.clone().context("wl_shm was not advertised")?;
    let output = state
        .output
        .clone()
        .context("wl_output was not advertised")?;
    let shell = state
        .layer_shell
        .clone()
        .context("wlr layer shell was not advertised")?;
    let surface = compositor.create_surface(&queue, ());
    let layer = shell.get_layer_surface(
        &surface,
        Some(&output),
        zwlr_layer_shell_v1::Layer::Top,
        "nobox-layer-probe".to_owned(),
        &queue,
        (),
    );
    layer.set_size(0, 32);
    layer.set_anchor(
        zwlr_layer_surface_v1::Anchor::Top
            | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right,
    );
    layer.set_exclusive_zone(32);
    surface.commit();
    state.surface = Some(surface);
    state.layer_surface = Some(layer);
    event_queue.roundtrip(&mut state)?;
    let (width, height) = state
        .configured_size
        .context("layer surface received no initial configure")?;
    ensure!(
        height == 32 && width > 0,
        "unexpected layer configure {width}x{height}"
    );
    let width_i32 = i32::try_from(width)?;
    let height_i32 = i32::try_from(height)?;
    let (file, buffer) = make_buffer(&shm, &queue, width_i32, height_i32)?;
    let surface = state.surface.clone().expect("stored above");
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, i32::try_from(width)?, i32::try_from(height)?);
    surface.frame(&queue, ());
    surface.commit();
    state.backing_file = Some(file);
    state.buffer = Some(buffer);
    event_queue.roundtrip(&mut state)?;
    ensure!(
        state.frame_done,
        "mapped layer surface received no frame callback"
    );
    surface.attach(None, 0, 0);
    surface.commit();
    event_queue.roundtrip(&mut state)?;
    println!("layer-shell-ok size={width}x{height} exclusive=32");
    Ok(())
}

#[derive(Default)]
struct LayerProbe {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    output: Option<wl_output::WlOutput>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    buffer: Option<wl_buffer::WlBuffer>,
    backing_file: Option<File>,
    configured_size: Option<(u32, u32)>,
    frame_done: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for LayerProbe {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => {
                state.compositor = Some(registry.bind(name, version.min(6), queue, ()))
            }
            "wl_shm" => state.shm = Some(registry.bind(name, version.min(1), queue, ())),
            "wl_output" => state.output = Some(registry.bind(name, version.min(4), queue, ())),
            "zwlr_layer_shell_v1" => {
                state.layer_shell = Some(registry.bind(name, version.min(5), queue, ()))
            }
            _ => {}
        }
    }
}

delegate_noop!(LayerProbe: ignore wl_compositor::WlCompositor);
delegate_noop!(LayerProbe: ignore wl_shm::WlShm);
delegate_noop!(LayerProbe: ignore wl_output::WlOutput);
delegate_noop!(LayerProbe: ignore wl_surface::WlSurface);
delegate_noop!(LayerProbe: ignore wl_buffer::WlBuffer);
delegate_noop!(LayerProbe: ignore wl_shm_pool::WlShmPool);
delegate_noop!(LayerProbe: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for LayerProbe {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer.ack_configure(serial);
                state.configured_size = Some((width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.layer_surface = None;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for LayerProbe {
    fn event(
        state: &mut Self,
        _callback: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_callback::Event::Done { .. }) {
            state.frame_done = true;
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProtocolViolation {
    Configure,
    FractionalScale,
    Role,
    Viewport,
}

#[derive(Clone, Copy)]
enum IdleNotificationKind {
    Standard,
    Input,
    Limit,
}

#[derive(Default)]
struct ShellProbe {
    compositor: Option<wl_compositor::WlCompositor>,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    shm: Option<wl_shm::WlShm>,
    outputs: Vec<wl_output::WlOutput>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    data_device_manager: Option<wl_data_device_manager::WlDataDeviceManager>,
    data_device: Option<wl_data_device::WlDataDevice>,
    data_source: Option<wl_data_source::WlDataSource>,
    replacement_data_source: Option<wl_data_source::WlDataSource>,
    pending_data_offer: Option<wl_data_offer::WlDataOffer>,
    clipboard_reader: Option<UnixStream>,
    clipboard_payload: Vec<u8>,
    clipboard_received: bool,
    clipboard_cancelled: bool,
    clipboard_cleared: bool,
    primary_selection_manager:
        Option<zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1>,
    primary_device: Option<zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1>,
    primary_source: Option<zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1>,
    replacement_primary_source:
        Option<zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1>,
    pending_primary_offer: Option<zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1>,
    primary_reader: Option<UnixStream>,
    primary_payload: Vec<u8>,
    primary_received: bool,
    primary_cancelled: bool,
    primary_cleared: bool,
    selection_replaced: bool,
    exercise_selection: bool,
    dnd_source: Option<wl_data_source::WlDataSource>,
    dnd_offer: Option<wl_data_offer::WlDataOffer>,
    dnd_reader: Option<UnixStream>,
    dnd_payload: Vec<u8>,
    dnd_received: bool,
    dnd_started: bool,
    dnd_entered: bool,
    dnd_dropped: bool,
    dnd_drop_performed: bool,
    dnd_finished: bool,
    dnd_cancelled: bool,
    dnd_mime_offered: bool,
    dnd_source_actions: Option<wl_data_device_manager::DndAction>,
    dnd_action: Option<wl_data_device_manager::DndAction>,
    dnd_icon_surface: Option<wl_surface::WlSurface>,
    dnd_icon_buffer: Option<wl_buffer::WlBuffer>,
    dnd_icon_backing_file: Option<File>,
    exercise_dnd: bool,
    xwayland_dnd_source: bool,
    pointer: Option<wl_pointer::WlPointer>,
    relative_pointer_manager: Option<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1>,
    relative_pointer: Option<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    relative_pointer_limit_objects: Vec<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    pointer_extension_limit: bool,
    pointer_constraints: Option<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1>,
    locked_pointer: Option<zwp_locked_pointer_v1::ZwpLockedPointerV1>,
    confined_pointer: Option<zwp_confined_pointer_v1::ZwpConfinedPointerV1>,
    pointer_probe_mode: Option<PointerProbeMode>,
    constraint_surface: Option<wl_surface::WlSurface>,
    constraint_active: bool,
    pointer_position: Option<(f64, f64)>,
    pointer_left: bool,
    relative_motion_count: usize,
    presentation: Option<wp_presentation::WpPresentation>,
    presentation_feedback: Option<wp_presentation_feedback::WpPresentationFeedback>,
    presentation_limit_feedbacks: Vec<wp_presentation_feedback::WpPresentationFeedback>,
    exercise_presentation: bool,
    presentation_limit: bool,
    presentation_clock_id: Option<u32>,
    presentation_presented: bool,
    presentation_discarded: bool,
    presentation_refresh: u32,
    presentation_sequence: u64,
    shortcut_inhibit_manager:
        Option<zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1>,
    shortcut_inhibitor:
        Option<zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1>,
    shortcut_inhibitor_limit_objects:
        Vec<zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1>,
    shortcut_inhibitor_limit_surfaces: Vec<wl_surface::WlSurface>,
    exercise_shortcut_inhibit: bool,
    shortcut_inhibitor_limit: bool,
    shortcut_inhibitor_active: bool,
    idle_inhibit_manager: Option<zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1>,
    idle_notifier: Option<ext_idle_notifier_v1::ExtIdleNotifierV1>,
    idle_inhibitor: Option<zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1>,
    idle_notification: Option<ext_idle_notification_v1::ExtIdleNotificationV1>,
    input_idle_notification: Option<ext_idle_notification_v1::ExtIdleNotificationV1>,
    idle_limit_inhibitors: Vec<zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1>,
    idle_limit_surfaces: Vec<wl_surface::WlSurface>,
    idle_limit_notifications: Vec<ext_idle_notification_v1::ExtIdleNotificationV1>,
    exercise_idle: bool,
    idle_inhibitor_limit: bool,
    idle_notification_limit: bool,
    standard_idled: bool,
    standard_resumed: bool,
    input_idled: bool,
    input_resumed: bool,
    pointer_gestures: Option<zwp_pointer_gestures_v1::ZwpPointerGesturesV1>,
    pointer_swipe_gestures: Vec<zwp_pointer_gesture_swipe_v1::ZwpPointerGestureSwipeV1>,
    pointer_pinch_gestures: Vec<zwp_pointer_gesture_pinch_v1::ZwpPointerGesturePinchV1>,
    pointer_hold_gestures: Vec<zwp_pointer_gesture_hold_v1::ZwpPointerGestureHoldV1>,
    exercise_pointer_gestures: bool,
    pointer_gesture_limit: bool,
    cursor_shape_manager: Option<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1>,
    cursor_shape_device: Option<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    cursor_shape_limit_devices: Vec<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    exercise_cursor_shape: bool,
    cursor_shape_limit: bool,
    text_input_manager: Option<zwp_text_input_manager_v3::ZwpTextInputManagerV3>,
    text_input: Option<zwp_text_input_v3::ZwpTextInputV3>,
    text_input_limit_objects: Vec<zwp_text_input_v3::ZwpTextInputV3>,
    exercise_text_input: bool,
    text_input_limit: bool,
    text_input_entered: bool,
    text_input_left: bool,
    text_input_done: bool,
    text_input_commit: Option<String>,
    saw_input_method_manager: bool,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    foreign_list: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    wlr_foreign_manager: Option<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1>,
    wlr_foreign_handle: Option<zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1>,
    wlr_foreign_title: Option<String>,
    wlr_foreign_app_id: Option<String>,
    wlr_foreign_done: bool,
    wlr_foreign_minimized: bool,
    wlr_foreign_activated: bool,
    wlr_foreign_outputs: usize,
    activation: Option<xdg_activation_v1::XdgActivationV1>,
    activation_token: Option<xdg_activation_token_v1::XdgActivationTokenV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    viewport: Option<wp_viewport::WpViewport>,
    fractional_scale_manager: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    fractional_scale: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    preferred_scale: Option<u32>,
    exercise_surface_protocols: bool,
    workspace_manager: Option<ext_workspace_manager_v1::ExtWorkspaceManagerV1>,
    workspaces: Vec<WorkspaceObservation>,
    workspace_done: bool,
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
    buffer_size: Option<(i32, i32)>,
    backing_file: Option<File>,
    configured: bool,
    configure_count: usize,
    frame_callbacks: usize,
    interaction_count: usize,
    key_events: usize,
    keycodes: Vec<u32>,
    close_received: bool,
    last_configure_size: Option<(i32, i32)>,
    respond_to_ping: bool,
    violation: Option<ProtocolViolation>,
    violation_sent: bool,
    request_popup_grab: bool,
    popup_grab_requested: bool,
    foreign_title: Option<String>,
    foreign_app_id: Option<String>,
    foreign_done: bool,
    foreign_closed: bool,
    pointer_enter_serial: Option<u32>,
    last_input_serial: Option<u32>,
    activation_done: bool,
    requested_title: Option<String>,
}

struct WorkspaceObservation {
    handle: ext_workspace_handle_v1::ExtWorkspaceHandleV1,
    name: Option<String>,
    active: bool,
}

impl ShellProbe {
    fn initialize(&mut self, queue: &QueueHandle<Self>) {
        if self.surface.is_none()
            && let Some(compositor) = &self.compositor
        {
            self.surface = Some(compositor.create_surface(queue, ()));
        }
        if self.viewport.is_none()
            && let (Some(viewporter), Some(surface)) = (&self.viewporter, &self.surface)
        {
            let viewport = viewporter.get_viewport(surface, queue, ());
            if self.exercise_surface_protocols {
                viewport.set_destination(160, 100);
            }
            if self.violation == Some(ProtocolViolation::Viewport) {
                let _duplicate = viewporter.get_viewport(surface, queue, ());
                self.violation_sent = true;
            }
            self.viewport = Some(viewport);
        }
        if self.fractional_scale.is_none()
            && let (Some(manager), Some(surface)) = (&self.fractional_scale_manager, &self.surface)
        {
            let fractional = manager.get_fractional_scale(surface, queue, ());
            if self.violation == Some(ProtocolViolation::FractionalScale) {
                let _duplicate = manager.get_fractional_scale(surface, queue, ());
                self.violation_sent = true;
            }
            self.fractional_scale = Some(fractional);
        }
        if self.data_device.is_none()
            && let (Some(manager), Some(seat)) = (&self.data_device_manager, &self.seat)
        {
            self.data_device = Some(manager.get_data_device(seat, queue, ()));
        }
        if self.primary_device.is_none()
            && let (Some(manager), Some(seat)) = (&self.primary_selection_manager, &self.seat)
        {
            self.primary_device = Some(manager.get_device(seat, queue, ()));
        }
        if self.relative_pointer.is_none()
            && self.pointer_probe_mode.is_some()
            && let (Some(manager), Some(pointer)) = (&self.relative_pointer_manager, &self.pointer)
        {
            self.relative_pointer = Some(manager.get_relative_pointer(pointer, queue, ()));
        }
        if self.pointer_extension_limit
            && self.relative_pointer_limit_objects.is_empty()
            && let (Some(manager), Some(pointer)) = (&self.relative_pointer_manager, &self.pointer)
        {
            for _ in 0..=nobox_wayland::MAX_CLIENT_POINTER_EXTENSION_OBJECTS {
                self.relative_pointer_limit_objects
                    .push(manager.get_relative_pointer(pointer, queue, ()));
            }
        }
        if self.presentation_limit
            && self.presentation_limit_feedbacks.is_empty()
            && let (Some(presentation), Some(surface)) = (&self.presentation, &self.surface)
        {
            for _ in 0..=nobox_wayland::MAX_CLIENT_PRESENTATION_FEEDBACKS {
                self.presentation_limit_feedbacks
                    .push(presentation.feedback(surface, queue, ()));
            }
        }
        if self.exercise_presentation
            && self.configured
            && self.presentation_feedback.is_none()
            && let (Some(presentation), Some(surface)) = (&self.presentation, &self.surface)
        {
            self.presentation_feedback = Some(presentation.feedback(surface, queue, ()));
            surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
            surface.commit();
        }
        if self.shortcut_inhibitor_limit
            && self.shortcut_inhibitor_limit_objects.is_empty()
            && let (Some(manager), Some(compositor), Some(seat)) =
                (&self.shortcut_inhibit_manager, &self.compositor, &self.seat)
        {
            for _ in 0..=nobox_wayland::MAX_CLIENT_SHORTCUT_INHIBITORS {
                let surface = compositor.create_surface(queue, ());
                let inhibitor = manager.inhibit_shortcuts(&surface, seat, queue, ());
                self.shortcut_inhibitor_limit_surfaces.push(surface);
                self.shortcut_inhibitor_limit_objects.push(inhibitor);
            }
        }
        if self.exercise_shortcut_inhibit
            && self.configured
            && self.shortcut_inhibitor.is_none()
            && let (Some(manager), Some(surface), Some(seat)) =
                (&self.shortcut_inhibit_manager, &self.surface, &self.seat)
        {
            self.shortcut_inhibitor = Some(manager.inhibit_shortcuts(surface, seat, queue, ()));
        }
        if self.idle_inhibitor_limit
            && self.idle_limit_inhibitors.is_empty()
            && let (Some(manager), Some(compositor)) =
                (&self.idle_inhibit_manager, &self.compositor)
        {
            for _ in 0..=nobox_wayland::MAX_CLIENT_IDLE_INHIBITORS {
                let surface = compositor.create_surface(queue, ());
                self.idle_limit_inhibitors
                    .push(manager.create_inhibitor(&surface, queue, ()));
                self.idle_limit_surfaces.push(surface);
            }
        }
        if self.idle_notification_limit
            && self.idle_limit_notifications.is_empty()
            && let (Some(notifier), Some(seat)) = (&self.idle_notifier, &self.seat)
        {
            for _ in 0..=nobox_wayland::MAX_CLIENT_IDLE_NOTIFICATIONS {
                self.idle_limit_notifications
                    .push(notifier.get_idle_notification(
                        60_000,
                        seat,
                        queue,
                        IdleNotificationKind::Limit,
                    ));
            }
        }
        if self.exercise_idle {
            if self.idle_inhibitor.is_none()
                && let (Some(manager), Some(surface)) = (&self.idle_inhibit_manager, &self.surface)
            {
                self.idle_inhibitor = Some(manager.create_inhibitor(surface, queue, ()));
            }
            if self.idle_notification.is_none()
                && let (Some(notifier), Some(seat)) = (&self.idle_notifier, &self.seat)
            {
                self.idle_notification = Some(notifier.get_idle_notification(
                    100,
                    seat,
                    queue,
                    IdleNotificationKind::Standard,
                ));
                self.input_idle_notification = Some(notifier.get_input_idle_notification(
                    100,
                    seat,
                    queue,
                    IdleNotificationKind::Input,
                ));
            }
        }
        if self.exercise_pointer_gestures
            && self.pointer_swipe_gestures.is_empty()
            && let (Some(manager), Some(pointer)) = (&self.pointer_gestures, &self.pointer)
        {
            if self.pointer_gesture_limit {
                for _ in 0..=nobox_wayland::MAX_CLIENT_POINTER_GESTURES {
                    self.pointer_swipe_gestures
                        .push(manager.get_swipe_gesture(pointer, queue, ()));
                }
            } else {
                self.pointer_swipe_gestures
                    .push(manager.get_swipe_gesture(pointer, queue, ()));
                self.pointer_pinch_gestures
                    .push(manager.get_pinch_gesture(pointer, queue, ()));
                self.pointer_hold_gestures
                    .push(manager.get_hold_gesture(pointer, queue, ()));
            }
        }
        if self.exercise_cursor_shape
            && self.cursor_shape_device.is_none()
            && let (Some(manager), Some(pointer)) = (&self.cursor_shape_manager, &self.pointer)
        {
            self.cursor_shape_device = Some(manager.get_pointer(pointer, queue, ()));
        }
        if self.cursor_shape_limit
            && self.cursor_shape_limit_devices.is_empty()
            && let (Some(manager), Some(pointer)) = (&self.cursor_shape_manager, &self.pointer)
        {
            for _ in 0..=nobox_wayland::MAX_CLIENT_CURSOR_SHAPES {
                self.cursor_shape_limit_devices
                    .push(manager.get_pointer(pointer, queue, ()));
            }
        }
        if self.text_input_limit
            && self.text_input_limit_objects.is_empty()
            && let (Some(manager), Some(seat)) = (&self.text_input_manager, &self.seat)
        {
            for _ in 0..=nobox_wayland::MAX_CLIENT_TEXT_INPUTS {
                self.text_input_limit_objects
                    .push(manager.get_text_input(seat, queue, ()));
            }
        } else if self.exercise_text_input
            && self.text_input.is_none()
            && let (Some(manager), Some(seat)) = (&self.text_input_manager, &self.seat)
        {
            self.text_input = Some(manager.get_text_input(seat, queue, ()));
        }
        if self.pointer_probe_mode.is_some()
            && self.locked_pointer.is_none()
            && self.confined_pointer.is_none()
            && let (Some(manager), Some(surface), Some(pointer)) = (
                &self.pointer_constraints,
                &self.constraint_surface,
                &self.pointer,
            )
        {
            match self.pointer_probe_mode {
                Some(PointerProbeMode::Lock) => {
                    self.locked_pointer = Some(manager.lock_pointer(
                        surface,
                        pointer,
                        None,
                        zwp_pointer_constraints_v1::Lifetime::Persistent,
                        queue,
                        (),
                    ));
                }
                Some(PointerProbeMode::Confine) => {
                    self.confined_pointer = Some(manager.confine_pointer(
                        surface,
                        pointer,
                        None,
                        zwp_pointer_constraints_v1::Lifetime::Persistent,
                        queue,
                        (),
                    ));
                }
                Some(PointerProbeMode::Duplicate) => {
                    self.locked_pointer = Some(manager.lock_pointer(
                        surface,
                        pointer,
                        None,
                        zwp_pointer_constraints_v1::Lifetime::Persistent,
                        queue,
                        (),
                    ));
                    self.confined_pointer = Some(manager.confine_pointer(
                        surface,
                        pointer,
                        None,
                        zwp_pointer_constraints_v1::Lifetime::Persistent,
                        queue,
                        (),
                    ));
                }
                None => {}
            }
        }
        if self.buffer.is_none()
            && let Some(shm) = &self.shm
        {
            let (file, buffer) =
                make_buffer(shm, queue, 160, 100).expect("create deterministic SHM buffer");
            self.backing_file = Some(file);
            self.buffer = Some(buffer);
            self.buffer_size = Some((160, 100));
        }
        if self.xdg_surface.is_none()
            && let (Some(wm_base), Some(surface)) = (&self.wm_base, &self.surface)
        {
            let xdg_surface = wm_base.get_xdg_surface(surface, queue, ShellSurface::Toplevel);
            let toplevel = xdg_surface.get_toplevel(queue, ());
            toplevel.set_title(
                self.requested_title
                    .clone()
                    .unwrap_or_else(|| "nobox deterministic shell probe".to_owned()),
            );
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

    fn begin_selection(&mut self, queue: &QueueHandle<Self>) {
        if !self.exercise_selection || !self.configured {
            return;
        }
        if self.data_source.is_none()
            && let (Some(manager), Some(device)) = (&self.data_device_manager, &self.data_device)
        {
            let source = manager.create_data_source(queue, ());
            source.offer("text/plain;charset=utf-8".to_owned());
            device.set_selection(Some(&source), 1);
            self.data_source = Some(source);
        }
        if self.primary_source.is_none()
            && let (Some(manager), Some(device)) =
                (&self.primary_selection_manager, &self.primary_device)
        {
            let source = manager.create_source(queue, ());
            source.offer("text/plain;charset=utf-8".to_owned());
            device.set_selection(Some(&source), 1);
            self.primary_source = Some(source);
        }
    }

    fn poll_selection(&mut self) -> Result<()> {
        self.poll_selection_payloads(b"nobox-clipboard", b"nobox-primary")
    }

    fn poll_selection_payloads(
        &mut self,
        clipboard_payload: &[u8],
        primary_payload: &[u8],
    ) -> Result<()> {
        self.clipboard_received |= poll_selection_pipe(
            &mut self.clipboard_reader,
            &mut self.clipboard_payload,
            clipboard_payload,
        )?;
        self.primary_received |= poll_selection_pipe(
            &mut self.primary_reader,
            &mut self.primary_payload,
            primary_payload,
        )?;
        Ok(())
    }

    fn replace_selection(&mut self, queue: &QueueHandle<Self>) {
        if let (Some(manager), Some(device)) = (&self.data_device_manager, &self.data_device) {
            let replacement = manager.create_data_source(queue, ());
            replacement.offer("text/plain;charset=utf-8".to_owned());
            device.set_selection(Some(&replacement), 2);
            self.replacement_data_source = Some(replacement);
        }
        if let (Some(manager), Some(device)) =
            (&self.primary_selection_manager, &self.primary_device)
        {
            let replacement = manager.create_source(queue, ());
            replacement.offer("text/plain;charset=utf-8".to_owned());
            device.set_selection(Some(&replacement), 2);
            self.replacement_primary_source = Some(replacement);
        }
        self.selection_replaced = true;
    }

    fn begin_dnd(&mut self, serial: u32, queue: &QueueHandle<Self>) {
        if !self.exercise_dnd || self.dnd_started {
            return;
        }
        let (Some(manager), Some(device), Some(compositor), Some(shm), Some(origin)) = (
            &self.data_device_manager,
            &self.data_device,
            &self.compositor,
            &self.shm,
            &self.surface,
        ) else {
            return;
        };
        let source = manager.create_data_source(queue, ());
        source.offer("text/plain;charset=utf-8".to_owned());
        source.set_actions(wl_data_device_manager::DndAction::Copy);
        let (file, buffer) = make_buffer(shm, queue, 16, 16).expect("create DND icon buffer");
        let icon = compositor.create_surface(queue, ());
        icon.attach(Some(&buffer), 0, 0);
        icon.damage_buffer(0, 0, 16, 16);
        icon.frame(queue, ());
        icon.commit();
        device.start_drag(Some(&source), origin, Some(&icon), serial);
        self.dnd_source = Some(source);
        self.dnd_icon_surface = Some(icon);
        self.dnd_icon_buffer = Some(buffer);
        self.dnd_icon_backing_file = Some(file);
        self.dnd_started = true;
    }

    fn poll_dnd(&mut self) -> Result<()> {
        self.dnd_received |=
            poll_selection_pipe(&mut self.dnd_reader, &mut self.dnd_payload, b"nobox-dnd")?;
        Ok(())
    }
}

fn make_buffer<D>(
    shm: &wl_shm::WlShm,
    queue: &QueueHandle<D>,
    width: i32,
    height: i32,
) -> Result<(File, wl_buffer::WlBuffer)>
where
    D: Dispatch<wl_shm_pool::WlShmPool, ()> + Dispatch<wl_buffer::WlBuffer, ()> + 'static,
{
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
                "wl_output" => state
                    .outputs
                    .push(registry.bind(name, version.min(4), queue, ())),
                "xdg_wm_base" => {
                    state.wm_base = Some(registry.bind(name, version.min(6), queue, ()));
                }
                "wl_seat" => state.seat = Some(registry.bind(name, version.min(9), queue, ())),
                "wl_data_device_manager" => {
                    state.data_device_manager = Some(registry.bind(name, version.min(3), queue, ()))
                }
                "ext_foreign_toplevel_list_v1" => {
                    state.foreign_list = Some(registry.bind(name, version.min(1), queue, ()));
                }
                "zwlr_foreign_toplevel_manager_v1" => {
                    state.wlr_foreign_manager =
                        Some(registry.bind(name, version.min(3), queue, ()));
                }
                "xdg_activation_v1" => {
                    state.activation = Some(registry.bind(name, version.min(1), queue, ()));
                }
                "wp_viewporter" => {
                    state.viewporter = Some(registry.bind(name, version.min(1), queue, ()));
                }
                "wp_fractional_scale_manager_v1" => {
                    state.fractional_scale_manager =
                        Some(registry.bind(name, version.min(1), queue, ()));
                }
                "zwp_relative_pointer_manager_v1" => {
                    state.relative_pointer_manager =
                        Some(registry.bind(name, version.min(1), queue, ()));
                }
                "zwp_pointer_constraints_v1" => {
                    state.pointer_constraints =
                        Some(registry.bind(name, version.min(1), queue, ()));
                }
                "zwp_pointer_gestures_v1" => {
                    state.pointer_gestures = Some(registry.bind(name, version.min(3), queue, ()));
                }
                "wp_cursor_shape_manager_v1" => {
                    state.cursor_shape_manager =
                        Some(registry.bind(name, version.min(2), queue, ()));
                }
                "zwp_text_input_manager_v3" => {
                    state.text_input_manager = Some(registry.bind(name, version.min(1), queue, ()))
                }
                "zwp_input_method_manager_v2" => state.saw_input_method_manager = true,
                "wp_presentation" => {
                    state.presentation = Some(registry.bind(name, version.min(2), queue, ()));
                }
                "zwp_keyboard_shortcuts_inhibit_manager_v1" => {
                    state.shortcut_inhibit_manager =
                        Some(registry.bind(name, version.min(1), queue, ()));
                }
                "zwp_idle_inhibit_manager_v1" => {
                    state.idle_inhibit_manager =
                        Some(registry.bind(name, version.min(1), queue, ()));
                }
                "ext_idle_notifier_v1" => {
                    state.idle_notifier = Some(registry.bind(name, version.min(2), queue, ()));
                }
                "zwp_primary_selection_device_manager_v1" => {
                    state.primary_selection_manager =
                        Some(registry.bind(name, version.min(1), queue, ()))
                }
                "ext_workspace_manager_v1" => {
                    state.workspace_manager = Some(registry.bind(name, version.min(1), queue, ()));
                }
                _ => {}
            }
            state.initialize(queue);
        }
    }
}

impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()> for ShellProbe {
    fn event(
        _state: &mut Self,
        _list: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        _event: ext_foreign_toplevel_list_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(ShellProbe, ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ())
    ]);
}

impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                state.foreign_title = Some(title);
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                state.foreign_app_id = Some(app_id);
            }
            ext_foreign_toplevel_handle_v1::Event::Done => state.foreign_done = true,
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                state.foreign_closed = true;
                handle.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, ()> for ShellProbe {
    fn event(
        _state: &mut Self,
        _manager: &zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
        _event: zwlr_foreign_toplevel_manager_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(ShellProbe, zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ())
    ]);
}

impl Dispatch<zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        handle: &zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        state.wlr_foreign_handle = Some(handle.clone());
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                state.wlr_foreign_title = Some(title);
            }
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                state.wlr_foreign_app_id = Some(app_id);
            }
            zwlr_foreign_toplevel_handle_v1::Event::State { state: bytes } => {
                let values = bytes
                    .chunks_exact(4)
                    .map(|bytes| u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                    .collect::<Vec<_>>();
                state.wlr_foreign_minimized =
                    values.contains(&(zwlr_foreign_toplevel_handle_v1::State::Minimized as u32));
                state.wlr_foreign_activated =
                    values.contains(&(zwlr_foreign_toplevel_handle_v1::State::Activated as u32));
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputEnter { .. } => {
                state.wlr_foreign_outputs = state.wlr_foreign_outputs.saturating_add(1);
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputLeave { .. } => {
                state.wlr_foreign_outputs = state.wlr_foreign_outputs.saturating_sub(1);
            }
            zwlr_foreign_toplevel_handle_v1::Event::Done => state.wlr_foreign_done = true,
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                state.wlr_foreign_handle = None;
                handle.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<ext_workspace_manager_v1::ExtWorkspaceManagerV1, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _manager: &ext_workspace_manager_v1::ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(event, ext_workspace_manager_v1::Event::Done) {
            state.workspace_done = true;
        }
    }

    wayland_client::event_created_child!(ShellProbe, ext_workspace_manager_v1::ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ext_workspace_handle_v1::ExtWorkspaceHandleV1, ())
    ]);
}

delegate_noop!(ShellProbe: ignore ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1);

impl Dispatch<ext_workspace_handle_v1::ExtWorkspaceHandleV1, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        handle: &ext_workspace_handle_v1::ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        let index = state
            .workspaces
            .iter()
            .position(|workspace| workspace.handle == *handle)
            .unwrap_or_else(|| {
                state.workspaces.push(WorkspaceObservation {
                    handle: handle.clone(),
                    name: None,
                    active: false,
                });
                state.workspaces.len() - 1
            });
        match event {
            ext_workspace_handle_v1::Event::Name { name } => {
                state.workspaces[index].name = Some(name);
            }
            ext_workspace_handle_v1::Event::State {
                state: WEnum::Value(workspace_state),
            } => {
                state.workspaces[index].active =
                    workspace_state.contains(ext_workspace_handle_v1::State::Active);
            }
            ext_workspace_handle_v1::Event::Removed => {
                state.workspaces.remove(index);
            }
            _ => {}
        }
    }
}

delegate_noop!(ShellProbe: ignore xdg_activation_v1::XdgActivationV1);
delegate_noop!(ShellProbe: ignore wl_data_device_manager::WlDataDeviceManager);

impl Dispatch<wl_data_device::WlDataDevice, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _device: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => state.pending_data_offer = Some(id),
            wl_data_device::Event::Enter {
                serial,
                id: Some(offer),
                ..
            } if state.exercise_dnd => {
                offer.accept(serial, Some("text/plain;charset=utf-8".to_owned()));
                offer.set_actions(
                    wl_data_device_manager::DndAction::Copy,
                    wl_data_device_manager::DndAction::Copy,
                );
                state.dnd_offer = Some(offer);
                state.dnd_entered = true;
            }
            wl_data_device::Event::Drop if state.exercise_dnd => {
                if let Some(offer) = state.dnd_offer.take() {
                    let (reader, writer) = UnixStream::pair().expect("create DND transfer pipe");
                    reader
                        .set_nonblocking(true)
                        .expect("make DND probe reader nonblocking");
                    offer.receive("text/plain;charset=utf-8".to_owned(), writer.as_fd());
                    drop(writer);
                    offer.finish();
                    offer.destroy();
                    state.dnd_reader = Some(reader);
                    state.dnd_dropped = true;
                }
            }
            wl_data_device::Event::Leave if state.exercise_dnd => {
                if let Some(offer) = state.dnd_offer.take() {
                    offer.destroy();
                }
            }
            wl_data_device::Event::Selection { id: Some(offer) }
                if !(state.selection_replaced && state.clipboard_received) =>
            {
                let (reader, writer) = UnixStream::pair().expect("create clipboard transfer pipe");
                reader
                    .set_nonblocking(true)
                    .expect("make clipboard probe reader nonblocking");
                offer.receive("text/plain;charset=utf-8".to_owned(), writer.as_fd());
                drop(writer);
                state.clipboard_reader = Some(reader);
            }
            wl_data_device::Event::Selection { id: None } => state.clipboard_cleared = true,
            _ => {}
        }
    }

    wayland_client::event_created_child!(ShellProbe, wl_data_device::WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, ())
    ]);
}

impl Dispatch<wl_data_offer::WlDataOffer, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _offer: &wl_data_offer::WlDataOffer,
        event: wl_data_offer::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_offer::Event::Offer { mime_type }
                if mime_type == "text/plain;charset=utf-8" =>
            {
                state.dnd_mime_offered = true;
            }
            wl_data_offer::Event::SourceActions {
                source_actions: WEnum::Value(actions),
            } => state.dnd_source_actions = Some(actions),
            wl_data_offer::Event::Action {
                dnd_action: WEnum::Value(action),
            } => state.dnd_action = Some(action),
            _ => {}
        }
    }
}

impl Dispatch<wl_data_source::WlDataSource, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        source: &wl_data_source::WlDataSource,
        event: wl_data_source::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { fd, .. } => {
                let mut file: File = fd.into();
                let payload = if state.dnd_source.as_ref() == Some(source) {
                    if state.xwayland_dnd_source {
                        b"nobox-cross-dnd".as_slice()
                    } else {
                        b"nobox-dnd".as_slice()
                    }
                } else {
                    b"nobox-clipboard".as_slice()
                };
                file.write_all(payload).expect("write data-source payload");
            }
            wl_data_source::Event::DndDropPerformed
                if state.dnd_source.as_ref() == Some(source) =>
            {
                state.dnd_drop_performed = true;
            }
            wl_data_source::Event::DndFinished if state.dnd_source.as_ref() == Some(source) => {
                state.dnd_finished = true;
            }
            wl_data_source::Event::Cancelled if state.dnd_source.as_ref() == Some(source) => {
                state.dnd_cancelled = true;
            }
            wl_data_source::Event::Cancelled if state.data_source.as_ref() == Some(source) => {
                state.clipboard_cancelled = true;
            }
            _ => {}
        }
    }
}

delegate_noop!(ShellProbe: ignore zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1);
delegate_noop!(ShellProbe: ignore zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1);

impl Dispatch<zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _device: &zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
        event: zwp_primary_selection_device_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwp_primary_selection_device_v1::Event::DataOffer { offer } => {
                state.pending_primary_offer = Some(offer);
            }
            zwp_primary_selection_device_v1::Event::Selection { id: Some(offer) } => {
                if state.selection_replaced && state.primary_received {
                    return;
                }
                let (reader, writer) = UnixStream::pair().expect("create primary transfer pipe");
                reader
                    .set_nonblocking(true)
                    .expect("make primary probe reader nonblocking");
                offer.receive("text/plain;charset=utf-8".to_owned(), writer.as_fd());
                drop(writer);
                state.primary_reader = Some(reader);
            }
            zwp_primary_selection_device_v1::Event::Selection { id: None } => {
                state.primary_cleared = true;
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(ShellProbe, zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1, [
        zwp_primary_selection_device_v1::EVT_DATA_OFFER_OPCODE => (zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1, ())
    ]);
}

impl Dispatch<zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        source: &zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
        event: zwp_primary_selection_source_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwp_primary_selection_source_v1::Event::Send { fd, .. } => {
                let mut file: File = fd.into();
                file.write_all(b"nobox-primary")
                    .expect("write primary-selection probe payload");
            }
            zwp_primary_selection_source_v1::Event::Cancelled
                if state.primary_source.as_ref() == Some(source) =>
            {
                state.primary_cancelled = true;
            }
            _ => {}
        }
    }
}

delegate_noop!(ShellProbe: ignore wp_viewporter::WpViewporter);
delegate_noop!(ShellProbe: ignore wp_viewport::WpViewport);
delegate_noop!(ShellProbe: ignore wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1);

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _fractional: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.preferred_scale = Some(scale);
        }
    }
}

impl Dispatch<xdg_activation_token_v1::XdgActivationTokenV1, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        token_proxy: &xdg_activation_token_v1::XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            state.activation_done = true;
            if let (Some(activation), Some(surface)) = (&state.activation, &state.surface) {
                activation.activate(token, surface);
            }
            token_proxy.destroy();
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
                    let configured_size = state.last_configure_size.unwrap_or((160, 100));
                    if state.buffer_size != Some(configured_size)
                        && let Some(shm) = &state.shm
                    {
                        let (file, buffer) =
                            make_buffer(shm, queue, configured_size.0, configured_size.1)
                                .expect("resize deterministic SHM buffer");
                        if let Some(previous) = state.buffer.take() {
                            previous.destroy();
                        }
                        state.backing_file = Some(file);
                        state.buffer = Some(buffer);
                        state.buffer_size = Some(configured_size);
                    }
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
            state.begin_selection(queue);
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
        if let wl_keyboard::Event::Key { key, .. } = event {
            state.key_events = state.key_events.saturating_add(1);
            state.keycodes.push(key);
        }
    }
}

delegate_noop!(ShellProbe: ignore zwp_text_input_manager_v3::ZwpTextInputManagerV3);

impl Dispatch<zwp_text_input_v3::ZwpTextInputV3, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        text_input: &zwp_text_input_v3::ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => {
                state.text_input_entered = true;
                text_input.enable();
                text_input.set_surrounding_text("hello".to_owned(), 5, 5);
                text_input.set_text_change_cause(zwp_text_input_v3::ChangeCause::InputMethod);
                text_input.set_content_type(
                    zwp_text_input_v3::ContentHint::Completion,
                    zwp_text_input_v3::ContentPurpose::Normal,
                );
                text_input.set_cursor_rectangle(12, 16, 2, 18);
                text_input.commit();
            }
            zwp_text_input_v3::Event::Leave { .. } => state.text_input_left = true,
            zwp_text_input_v3::Event::CommitString { text } => {
                state.text_input_commit = text;
            }
            zwp_text_input_v3::Event::Done { .. } => state.text_input_done = true,
            _ => {}
        }
    }
}

delegate_noop!(ShellProbe: ignore zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1);

impl Dispatch<wp_presentation::WpPresentation, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _presentation: &wp_presentation::WpPresentation,
        event: wp_presentation::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.presentation_clock_id = Some(clk_id);
        }
    }
}

impl Dispatch<wp_presentation_feedback::WpPresentationFeedback, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _feedback: &wp_presentation_feedback::WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                refresh,
                seq_hi,
                seq_lo,
                ..
            } => {
                state.presentation_presented = true;
                state.presentation_refresh = refresh;
                state.presentation_sequence = (u64::from(seq_hi) << 32) | u64::from(seq_lo);
            }
            wp_presentation_feedback::Event::Discarded => {
                state.presentation_discarded = true;
            }
            _ => {}
        }
    }
}

delegate_noop!(ShellProbe: ignore zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1);
delegate_noop!(ShellProbe: ignore zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1);
delegate_noop!(ShellProbe: ignore zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1);
delegate_noop!(ShellProbe: ignore ext_idle_notifier_v1::ExtIdleNotifierV1);

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, IdleNotificationKind>
    for ShellProbe
{
    fn event(
        state: &mut Self,
        _notification: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        kind: &IdleNotificationKind,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match (kind, event) {
            (IdleNotificationKind::Standard, ext_idle_notification_v1::Event::Idled) => {
                state.standard_idled = true;
            }
            (IdleNotificationKind::Standard, ext_idle_notification_v1::Event::Resumed) => {
                state.standard_idled = false;
                state.standard_resumed = true;
            }
            (IdleNotificationKind::Input, ext_idle_notification_v1::Event::Idled) => {
                state.input_idled = true;
            }
            (IdleNotificationKind::Input, ext_idle_notification_v1::Event::Resumed) => {
                state.input_idled = false;
                state.input_resumed = true;
            }
            (IdleNotificationKind::Limit, _) | (_, _) => {}
        }
    }
}

impl Dispatch<zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1, ()>
    for ShellProbe
{
    fn event(
        state: &mut Self,
        _inhibitor: &zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => {
                state.shortcut_inhibitor_active = true;
            }
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => {
                state.shortcut_inhibitor_active = false;
            }
            _ => {}
        }
    }
}

impl Dispatch<zwp_relative_pointer_v1::ZwpRelativePointerV1, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _pointer: &zwp_relative_pointer_v1::ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(event, zwp_relative_pointer_v1::Event::RelativeMotion { .. }) {
            state.relative_motion_count = state.relative_motion_count.saturating_add(1);
        }
    }
}

delegate_noop!(ShellProbe: ignore zwp_pointer_constraints_v1::ZwpPointerConstraintsV1);
delegate_noop!(ShellProbe: ignore zwp_pointer_gestures_v1::ZwpPointerGesturesV1);
delegate_noop!(ShellProbe: ignore zwp_pointer_gesture_swipe_v1::ZwpPointerGestureSwipeV1);
delegate_noop!(ShellProbe: ignore zwp_pointer_gesture_pinch_v1::ZwpPointerGesturePinchV1);
delegate_noop!(ShellProbe: ignore zwp_pointer_gesture_hold_v1::ZwpPointerGestureHoldV1);
delegate_noop!(ShellProbe: ignore wp_cursor_shape_manager_v1::WpCursorShapeManagerV1);
delegate_noop!(ShellProbe: ignore wp_cursor_shape_device_v1::WpCursorShapeDeviceV1);

impl Dispatch<zwp_locked_pointer_v1::ZwpLockedPointerV1, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _pointer: &zwp_locked_pointer_v1::ZwpLockedPointerV1,
        event: zwp_locked_pointer_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => state.constraint_active = true,
            zwp_locked_pointer_v1::Event::Unlocked => state.constraint_active = false,
            _ => {}
        }
    }
}

impl Dispatch<zwp_confined_pointer_v1::ZwpConfinedPointerV1, ()> for ShellProbe {
    fn event(
        state: &mut Self,
        _pointer: &zwp_confined_pointer_v1::ZwpConfinedPointerV1,
        event: zwp_confined_pointer_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwp_confined_pointer_v1::Event::Confined => state.constraint_active = true,
            zwp_confined_pointer_v1::Event::Unconfined => state.constraint_active = false,
            _ => {}
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
        if let wl_pointer::Event::Enter {
            serial,
            surface,
            surface_x,
            surface_y,
        } = event
        {
            state.pointer_position = Some((surface_x, surface_y));
            state.constraint_surface = Some(surface);
            state.pointer_enter_serial = Some(serial);
            if state.cursor_surface.is_none()
                && !state.exercise_cursor_shape
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
            state.initialize(queue);
            return;
        }
        if let wl_pointer::Event::Motion {
            surface_x,
            surface_y,
            ..
        } = event
        {
            state.pointer_position = Some((surface_x, surface_y));
            return;
        }
        if matches!(event, wl_pointer::Event::Leave { .. }) {
            state.pointer_left = true;
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
        state.last_input_serial = Some(serial);
        let (Some(seat), Some(toplevel)) = (&state.seat, &state.toplevel) else {
            return;
        };
        if state.exercise_dnd {
            state.begin_dnd(serial, queue);
            return;
        }
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
    inject_parent_input_with_layout(events, true)
}

fn inject_parent_surface_input(events: &[(u8, u8, i16, i16)]) -> Result<()> {
    inject_parent_input_with_layout(events, false)
}

fn inject_parent_input_with_layout(
    events: &[(u8, u8, i16, i16)],
    center_logical_desktop: bool,
) -> Result<()> {
    let (connection, screen) = x11rb::connect(None)?;
    let root = connection.setup().roots[screen].root;
    let (nested_window, nested_width, nested_height) = connection
        .query_tree(root)?
        .reply()?
        .children
        .into_iter()
        .filter_map(|window| {
            let attributes = connection
                .get_window_attributes(window)
                .ok()?
                .reply()
                .ok()?;
            let geometry = connection.get_geometry(window).ok()?.reply().ok()?;
            (attributes.map_state == MapState::VIEWABLE).then_some((
                window,
                geometry.width,
                geometry.height,
                u32::from(geometry.width).saturating_mul(u32::from(geometry.height)),
            ))
        })
        .max_by_key(|(_, _, _, area)| *area)
        .map(|(window, width, height, _)| (window, width, height))
        .context("nested compositor X11 window is not viewable")?;
    let origin = connection
        .translate_coordinates(nested_window, root, 0, 0)?
        .reply()?;
    if events
        .iter()
        .any(|(type_, _, _, _)| matches!(*type_, KEY_PRESS_EVENT | KEY_RELEASE_EVENT))
    {
        connection
            .set_input_focus(InputFocus::PARENT, nested_window, CURRENT_TIME)?
            .check()?;
    }
    for &(type_, detail, x, y) in events {
        let center_offset_x = if center_logical_desktop {
            i16::try_from(nested_width / 2)
                .unwrap_or(i16::MAX)
                .saturating_sub(320)
        } else {
            0
        };
        let center_offset_y = if center_logical_desktop {
            i16::try_from(nested_height / 2)
                .unwrap_or(i16::MAX)
                .saturating_sub(180)
        } else {
            0
        };
        let (x, y) = if matches!(
            type_,
            MOTION_NOTIFY_EVENT | BUTTON_PRESS_EVENT | BUTTON_RELEASE_EVENT
        ) {
            (
                x.saturating_add(center_offset_x)
                    .saturating_add(origin.dst_x),
                y.saturating_add(center_offset_y)
                    .saturating_add(origin.dst_y),
            )
        } else {
            (x, y)
        };
        connection
            .xtest_fake_input(type_, detail, CURRENT_TIME, root, x, y, 0)?
            .check()?;
    }
    connection.flush()?;
    Ok(())
}

fn parent_center_pixel() -> Result<[u8; 4]> {
    let (connection, screen) = x11rb::connect(None)?;
    let root = connection.setup().roots[screen].root;
    let (window, width, height) = connection
        .query_tree(root)?
        .reply()?
        .children
        .into_iter()
        .filter_map(|window| {
            let attributes = connection
                .get_window_attributes(window)
                .ok()?
                .reply()
                .ok()?;
            let geometry = connection.get_geometry(window).ok()?.reply().ok()?;
            (attributes.map_state == MapState::VIEWABLE).then_some((
                window,
                geometry.width,
                geometry.height,
                u32::from(geometry.width).saturating_mul(u32::from(geometry.height)),
            ))
        })
        .max_by_key(|(_, _, _, area)| *area)
        .map(|(window, width, height, _)| (window, width, height))
        .context("nested compositor X11 window is not viewable")?;
    let image = connection
        .get_image(
            x11rb::protocol::xproto::ImageFormat::Z_PIXMAP,
            window,
            i16::try_from(width / 2).unwrap_or(i16::MAX),
            i16::try_from(height / 2).unwrap_or(i16::MAX),
            1,
            1,
            u32::MAX,
        )?
        .reply()?;
    image
        .data
        .get(..4)
        .and_then(|pixel| <[u8; 4]>::try_from(pixel).ok())
        .context("nested compositor center pixel was incomplete")
}

fn parent_pixels() -> Result<Vec<u8>> {
    let (connection, screen) = x11rb::connect(None)?;
    let root = connection.setup().roots[screen].root;
    let (window, width, height) = connection
        .query_tree(root)?
        .reply()?
        .children
        .into_iter()
        .filter_map(|window| {
            let attributes = connection
                .get_window_attributes(window)
                .ok()?
                .reply()
                .ok()?;
            let geometry = connection.get_geometry(window).ok()?.reply().ok()?;
            (attributes.map_state == MapState::VIEWABLE).then_some((
                window,
                geometry.width,
                geometry.height,
                u32::from(geometry.width).saturating_mul(u32::from(geometry.height)),
            ))
        })
        .max_by_key(|(_, _, _, area)| *area)
        .map(|(window, width, height, _)| (window, width, height))
        .context("nested compositor X11 window is not viewable")?;
    let root_geometry = connection.get_geometry(root)?.reply()?;
    let width = width.min(root_geometry.width);
    let height = height.min(root_geometry.height);
    Ok(connection
        .get_image(
            x11rb::protocol::xproto::ImageFormat::Z_PIXMAP,
            window,
            0,
            0,
            width,
            height,
            u32::MAX,
        )?
        .reply()?
        .data)
}

fn pixel_differences(left: &[u8], right: &[u8]) -> usize {
    left.chunks_exact(4)
        .zip(right.chunks_exact(4))
        .filter(|(left, right)| left != right)
        .count()
}

delegate_noop!(ShellProbe: ignore wl_compositor::WlCompositor);
delegate_noop!(ShellProbe: ignore wl_surface::WlSurface);
delegate_noop!(ShellProbe: ignore wl_shm::WlShm);
delegate_noop!(ShellProbe: ignore wl_shm_pool::WlShmPool);
delegate_noop!(ShellProbe: ignore wl_buffer::WlBuffer);
delegate_noop!(ShellProbe: ignore wl_output::WlOutput);
delegate_noop!(ShellProbe: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(ShellProbe: ignore wl_subsurface::WlSubsurface);
delegate_noop!(ShellProbe: ignore xdg_positioner::XdgPositioner);
delegate_noop!(ShellProbe: ignore xdg_popup::XdgPopup);
