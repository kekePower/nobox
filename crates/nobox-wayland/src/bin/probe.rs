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
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry,
        wl_seat, wl_shm, wl_shm_pool, wl_subcompositor, wl_subsurface, wl_surface,
    },
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1, ext_workspace_handle_v1, ext_workspace_manager_v1,
};
use wayland_protocols::xdg::activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1};
use wayland_protocols::xdg::shell::client::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
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
        Some("--unresponsive") => return probe_unresponsive(),
        Some("--close") => return probe_close(),
        Some("--decoration-close") => return probe_decoration_close(),
        Some("--keyboard-resize") => return probe_keyboard_resize(),
        Some("--mouse-resize") => return probe_mouse_resize(),
        Some("--focus-cycle") => return probe_focus_cycle(),
        Some("--popup-grab") => return probe_popup_grab(),
        Some("--layer-shell") => return probe_layer_shell(),
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
    foreign_list: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    activation: Option<xdg_activation_v1::XdgActivationV1>,
    activation_token: Option<xdg_activation_token_v1::XdgActivationTokenV1>,
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
    last_input_serial: Option<u32>,
    activation_done: bool,
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
                "xdg_wm_base" => {
                    state.wm_base = Some(registry.bind(name, version.min(6), queue, ()));
                }
                "wl_seat" => state.seat = Some(registry.bind(name, version.min(9), queue, ())),
                "ext_foreign_toplevel_list_v1" => {
                    state.foreign_list = Some(registry.bind(name, version.min(1), queue, ()));
                }
                "xdg_activation_v1" => {
                    state.activation = Some(registry.bind(name, version.min(1), queue, ()));
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
        if let wl_keyboard::Event::Key { key, .. } = event {
            state.key_events = state.key_events.saturating_add(1);
            state.keycodes.push(key);
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
        state.last_input_serial = Some(serial);
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
        let center_offset_x = i16::try_from(nested_width / 2)
            .unwrap_or(i16::MAX)
            .saturating_sub(320);
        let center_offset_y = i16::try_from(nested_height / 2)
            .unwrap_or(i16::MAX)
            .saturating_sub(180);
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

delegate_noop!(ShellProbe: ignore wl_compositor::WlCompositor);
delegate_noop!(ShellProbe: ignore wl_surface::WlSurface);
delegate_noop!(ShellProbe: ignore wl_shm::WlShm);
delegate_noop!(ShellProbe: ignore wl_shm_pool::WlShmPool);
delegate_noop!(ShellProbe: ignore wl_buffer::WlBuffer);
delegate_noop!(ShellProbe: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(ShellProbe: ignore wl_subsurface::WlSubsurface);
delegate_noop!(ShellProbe: ignore xdg_positioner::XdgPositioner);
delegate_noop!(ShellProbe: ignore xdg_popup::XdgPopup);
