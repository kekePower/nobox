//! Native Wayland Nobox session executable.

use std::path::Path;

use anyhow::{Context, Result, bail};
use nobox_common::{
    Backend, BackendDriver, PanelSupervisor, SignalForwarder, launch_autostart_wayland,
    load_or_default, load_session_restore, parse_instance, replace_with_command, run_backend,
};
use nobox_config::state_path;
use nobox_runtime::{BackendCapabilities, BackendKind, RunDisposition, RunningInstance};
use tracing::{info, warn};

struct WaylandDriver;

impl BackendDriver for WaylandDriver {
    const BACKEND: Backend = Backend::Wayland;
    const BINARY_NAME: &'static str = "nobox-wayland";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    fn exit(_display: Option<&str>, instance: Option<&str>) -> Result<()> {
        let running = match instance {
            Some(instance) => {
                RunningInstance::load(BackendKind::Wayland, &parse_instance(instance)?)
            }
            None => RunningInstance::discover_unique(BackendKind::Wayland),
        }
        .context("failed to locate one unambiguous Wayland nobox instance")?;
        running
            .sender()
            .context("failed to validate the running nobox instance")?
            .shutdown()
            .context("failed to request a clean nobox exit")
    }

    fn run(
        path: &Path,
        display: Option<&str>,
        no_autostart: bool,
        nested_x11: bool,
        tty: bool,
        sm_client_id: Option<&str>,
    ) -> Result<()> {
        if sm_client_id.is_some() {
            bail!("--sm-client-id is only valid with the X11 backend");
        }
        if tty && display.is_some() {
            bail!("--display is not valid with the direct Wayland --tty path");
        }
        if tty {
            return run_direct(path, no_autostart);
        }
        if !nested_x11 {
            bail!("select --nested-x11 for isolated Wayland or --tty for direct libseat/DRM");
        }
        run_nested(path, display, no_autostart)
    }

    fn doctor(path: &Path, display: Option<&str>, nested_x11: bool, _tty: bool) -> Result<()> {
        if nested_x11 {
            doctor_nested(display)
        } else {
            doctor_direct(path)
        }
    }
}

fn main() -> Result<()> {
    run_backend::<WaylandDriver>()
}

fn run_nested(path: &Path, display: Option<&str>, no_autostart: bool) -> Result<()> {
    let session_path = state_path()?;
    let mut restore = load_session_restore(&session_path);
    let mut initial_start = true;
    let options = nobox_wayland::NestedOptions {
        display: display.map(str::to_owned),
        ..nobox_wayland::NestedOptions::default()
    };
    let panel = PanelSupervisor::new(
        path,
        Some(&options.socket_name),
        BackendCapabilities::WAYLAND_NESTED,
    );

    loop {
        let config = load_or_default(path)?;
        if initial_start && !no_autostart {
            nobox_common::launch_autostart(path)?;
        }
        initial_start = false;
        let panel_config = config.clone();
        let report = nobox_wayland::run_nested_with_session(
            options.clone(),
            config,
            restore,
            |control| {
                let signals = SignalForwarder::install(control)?;
                panel.sync(&panel_config);
                Ok::<_, anyhow::Error>(signals)
            },
            || -> Result<_> {
                let config = load_or_default(path)?;
                panel.sync(&config);
                Ok(config)
            },
            |snapshot| match snapshot.save(&session_path) {
                Ok(()) => true,
                Err(error) => {
                    warn!(%error, path = %session_path.display(), "could not save requested Wayland snapshot");
                    false
                }
            },
        )
        .context("Wayland event loop stopped")?;
        info!(
            socket = %report.socket_name.to_string_lossy(),
            frames = report.rendered_frames,
            disconnected_clients = report.disconnected_clients,
            renderer = ?report.renderer,
            "nested Wayland backend stopped cleanly"
        );
        let (snapshot, disposition) = report.into_parts();
        if let Err(error) = snapshot.save(&session_path) {
            warn!(%error, path = %session_path.display(), "could not save session state");
        }
        panel.stop();
        match disposition {
            RunDisposition::Exit => return Ok(()),
            RunDisposition::Restart { command: None } => {
                info!("restarting the Wayland compositor without rerunning autostart");
                restore = snapshot.into_restore();
            }
            RunDisposition::Restart {
                command: Some(command),
            } => return replace_with_command(&command),
        }
    }
}

fn run_direct(path: &Path, no_autostart: bool) -> Result<()> {
    let session_path = state_path()?;
    let mut restore = load_session_restore(&session_path);
    let mut initial_start = true;
    let options = nobox_wayland::DirectOptions::default();
    let socket_name = options.socket_name.clone();
    let panel = PanelSupervisor::new(
        path,
        Some(&socket_name),
        BackendCapabilities::WAYLAND_NESTED,
    );

    loop {
        let config = load_or_default(path)?;
        let panel_config = config.clone();
        let launch_session = initial_start && !no_autostart;
        initial_start = false;
        let report = nobox_wayland::run_direct_with_session(
            options.clone(),
            config,
            restore,
            |control| {
                let signals = SignalForwarder::install(control)?;
                panel.sync(&panel_config);
                Ok::<_, anyhow::Error>(signals)
            },
            |xwayland_display| {
                if launch_session {
                    launch_autostart_wayland(path, &socket_name, xwayland_display)?;
                }
                Ok::<_, anyhow::Error>(())
            },
            || -> Result<_> {
                let config = load_or_default(path)?;
                panel.sync(&config);
                Ok(config)
            },
            |snapshot| match snapshot.save(&session_path) {
                Ok(()) => true,
                Err(error) => {
                    warn!(%error, path = %session_path.display(), "could not save requested direct Wayland snapshot");
                    false
                }
            },
        )
        .context("direct Wayland event loop stopped")?;
        info!(
            socket = %report.socket_name.to_string_lossy(),
            frames = report.rendered_frames,
            disconnected_clients = report.disconnected_clients,
            "direct Wayland backend stopped cleanly"
        );
        let (snapshot, disposition) = report.into_parts();
        if let Err(error) = snapshot.save(&session_path) {
            warn!(%error, path = %session_path.display(), "could not save direct Wayland session state");
        }
        panel.stop();
        match disposition {
            RunDisposition::Exit => return Ok(()),
            RunDisposition::Restart { command: None } => {
                info!("restarting the direct Wayland compositor without rerunning autostart");
                restore = snapshot.into_restore();
            }
            RunDisposition::Restart {
                command: Some(command),
            } => return replace_with_command(&command),
        }
    }
}

fn doctor_nested(display: Option<&str>) -> Result<()> {
    let diagnostics = nobox_wayland::NestedDiagnostics::inspect(display)?;
    let capabilities = BackendCapabilities::WAYLAND_NESTED;
    println!(
        "[ok] Wayland backend: Smithay {} (managed nested shell)",
        nobox_wayland::SMITHAY_VERSION
    );
    println!("[ok] nested X11 display: {}", diagnostics.display);
    println!(
        "[ok] private Wayland runtime directory: {}",
        diagnostics.runtime_dir.display()
    );
    println!("[ok] renderers: Smithay GLES2 with Pixman fallback");
    print_protocol_diagnostics();
    println!(
        "[info] backend capabilities: nested-x11={}, direct={}, session-restore={}, panel={}, agent-seat={}",
        capabilities.nested_x11,
        capabilities.direct_session,
        capabilities.session_restore,
        capabilities.panel,
        capabilities.agent_seat
    );
    println!("ready: yes (managed nested-X11 Wayland shell)");
    Ok(())
}

fn doctor_direct(path: &Path) -> Result<()> {
    let _config = load_or_default(path)?;
    let diagnostics = nobox_wayland::DirectDiagnostics::inspect()?;
    let capabilities = BackendCapabilities::WAYLAND_NESTED;
    println!(
        "[ok] Wayland backend: Smithay {} (direct-session prerequisites)",
        nobox_wayland::SMITHAY_VERSION
    );
    println!("[ok] libseat backend: {}", diagnostics.libseat_backend);
    println!("[ok] seat: {}", diagnostics.seat);
    if let Some(session) = &diagnostics.session_id {
        println!("[ok] logind session: {session}");
    } else {
        println!("[info] logind session: not exported; libseat may select seatd");
    }
    if let Some(session_type) = &diagnostics.session_type {
        println!("[info] current session type: {session_type}");
    }
    println!(
        "[ok] private Wayland runtime directory: {}",
        diagnostics.runtime_directory.display()
    );
    println!(
        "[info] direct protocols: zwp_linux_dmabuf_v1 v{}; wp_linux_drm_syncobj_manager_v1 v{} when syncobj-eventfd is supported",
        nobox_wayland::LINUX_DMABUF_VERSION,
        nobox_wayland::LINUX_DRM_SYNCOBJ_VERSION
    );
    print_protocol_diagnostics();
    for device in &diagnostics.drm_devices {
        println!(
            "[{}] DRM card: {}",
            if device.accessible { "ok" } else { "warn" },
            device.path.display()
        );
    }
    for device in &diagnostics.render_devices {
        println!(
            "[{}] DRM render node: {}",
            if device.accessible { "ok" } else { "warn" },
            device.path.display()
        );
    }
    println!(
        "[ok] libinput event nodes discovered: {}",
        diagnostics.input_devices.len()
    );
    if let Some(xwayland) = &diagnostics.xwayland {
        println!("[info] optional XWayland: {}", xwayland.display());
    } else {
        println!("[info] optional XWayland: not found (W7 remains optional)");
    }
    println!(
        "[info] backend capabilities: nested-x11={}, direct={}, session-restore={}, panel={}, agent-seat={}",
        capabilities.nested_x11,
        capabilities.direct_session,
        capabilities.session_restore,
        capabilities.panel,
        capabilities.agent_seat
    );
    if diagnostics.ready() {
        println!("ready: yes (direct-session prerequisites; hardware acceptance pending)");
        Ok(())
    } else {
        println!("ready: no (missing direct-session device prerequisites)");
        bail!("direct Wayland prerequisites are incomplete")
    }
}

fn print_protocol_diagnostics() {
    println!(
        "[info] surface protocols: wp_viewporter v{}; wp_fractional_scale_manager_v1 v{}",
        nobox_wayland::VIEWPORTER_VERSION,
        nobox_wayland::FRACTIONAL_SCALE_VERSION
    );
    println!(
        "[info] core resource limits per client: {} SHM pools ({} MiB each); {} SHM buffers ({} px/axis); {} frame callbacks; {} XDG positioners; {} XDG popups; {} pending configures/surface",
        nobox_wayland::MAX_CLIENT_SHM_POOLS,
        nobox_wayland::MAX_SHM_POOL_BYTES / (1024 * 1024),
        nobox_wayland::MAX_CLIENT_SHM_BUFFERS,
        nobox_wayland::MAX_SHM_BUFFER_DIMENSION,
        nobox_wayland::MAX_CLIENT_FRAME_CALLBACKS,
        nobox_wayland::MAX_CLIENT_XDG_POSITIONERS,
        nobox_wayland::MAX_CLIENT_XDG_POPUPS,
        nobox_wayland::MAX_PENDING_XDG_CONFIGURES,
    );
    println!(
        "[info] panel protocols: zwlr_layer_shell_v1 v{}; ext_foreign_toplevel_list_v1 v{}; ext_workspace_manager_v1 v{}; zwlr_foreign_toplevel_manager_v1 v{} ({} managers/client)",
        nobox_wayland::LAYER_SHELL_VERSION,
        nobox_wayland::FOREIGN_TOPLEVEL_LIST_VERSION,
        nobox_wayland::WORKSPACE_MANAGER_VERSION,
        nobox_wayland::WLR_FOREIGN_TOPLEVEL_MANAGER_VERSION,
        nobox_wayland::MAX_CLIENT_FOREIGN_TOPLEVEL_MANAGERS
    );
    println!(
        "[info] selection protocols: wl_data_device_manager v{}; zwp_primary_selection_device_manager_v1 v{}",
        nobox_wayland::DATA_DEVICE_VERSION,
        nobox_wayland::PRIMARY_SELECTION_VERSION
    );
    println!(
        "[info] selection limits per client: {} sources; {} devices; {} MIME types/source; {} bytes/MIME type",
        nobox_wayland::MAX_CLIENT_SELECTION_SOURCES,
        nobox_wayland::MAX_CLIENT_SELECTION_DEVICES,
        nobox_wayland::MAX_SOURCE_MIME_TYPES,
        nobox_wayland::MAX_MIME_TYPE_BYTES
    );
    println!(
        "[info] pointer protocols: zwp_relative_pointer_manager_v{}; zwp_pointer_constraints_v1 v{}; zwp_pointer_gestures_v1 v{}; wp_cursor_shape_manager_v1 v{}; {} extension objects/client; {} gesture objects/client; {} cursor-shape devices/client",
        nobox_wayland::RELATIVE_POINTER_VERSION,
        nobox_wayland::POINTER_CONSTRAINTS_VERSION,
        nobox_wayland::POINTER_GESTURES_VERSION,
        nobox_wayland::CURSOR_SHAPE_VERSION,
        nobox_wayland::MAX_CLIENT_POINTER_EXTENSION_OBJECTS,
        nobox_wayland::MAX_CLIENT_POINTER_GESTURES,
        nobox_wayland::MAX_CLIENT_CURSOR_SHAPES
    );
    println!(
        "[info] touch protocol: wl_touch via wl_seat v9; {} touch devices/client",
        nobox_wayland::MAX_CLIENT_TOUCH_DEVICES
    );
    println!(
        "[info] tablet protocol: zwp_tablet_manager_v2 v{}; {} tablet seats/client; {} tablets/seat; {} tools/seat; {} pads/seat; {}/{}/{} groups/rings/strips per pad; deterministic removal",
        nobox_wayland::TABLET_MANAGER_VERSION,
        nobox_wayland::MAX_CLIENT_TABLET_SEATS,
        nobox_wayland::MAX_TABLET_DEVICES,
        nobox_wayland::MAX_TABLET_TOOLS,
        nobox_wayland::MAX_TABLET_PADS,
        nobox_wayland::MAX_TABLET_PAD_GROUPS,
        nobox_wayland::MAX_TABLET_PAD_RINGS,
        nobox_wayland::MAX_TABLET_PAD_STRIPS
    );
    println!(
        "[info] text input protocols when [wayland].input_method is configured: zwp_text_input_manager_v3 v{}; private zwp_input_method_manager_v2 v{}; {} text inputs/client; {} input-method objects/authorized connection; {} popups and {} keyboard grabs/input method",
        nobox_wayland::TEXT_INPUT_MANAGER_VERSION,
        nobox_wayland::INPUT_METHOD_MANAGER_VERSION,
        nobox_wayland::MAX_CLIENT_TEXT_INPUTS,
        nobox_wayland::MAX_CLIENT_INPUT_METHODS,
        nobox_wayland::MAX_CLIENT_INPUT_METHOD_POPUPS,
        nobox_wayland::MAX_CLIENT_INPUT_METHOD_KEYBOARD_GRABS
    );
    println!(
        "[info] timing protocol: wp_presentation v{}; {} feedbacks/client",
        nobox_wayland::PRESENTATION_VERSION,
        nobox_wayland::MAX_CLIENT_PRESENTATION_FEEDBACKS
    );
    println!(
        "[info] inhibition and idle protocols: zwp_keyboard_shortcuts_inhibit_manager_v1 v{} ({} inhibitors/client); zwp_idle_inhibit_manager_v1 v{} ({} inhibitors/client); ext_idle_notifier_v1 v{} ({} notifications/client)",
        nobox_wayland::KEYBOARD_SHORTCUTS_INHIBIT_VERSION,
        nobox_wayland::MAX_CLIENT_SHORTCUT_INHIBITORS,
        nobox_wayland::IDLE_INHIBIT_VERSION,
        nobox_wayland::MAX_CLIENT_IDLE_INHIBITORS,
        nobox_wayland::IDLE_NOTIFY_VERSION,
        nobox_wayland::MAX_CLIENT_IDLE_NOTIFICATIONS
    );
    println!(
        "[info] session lock protocol: ext_session_lock_manager_v1 v{}; {} locks/client; {} lock surfaces/client",
        nobox_wayland::SESSION_LOCK_VERSION,
        nobox_wayland::MAX_CLIENT_SESSION_LOCKS,
        nobox_wayland::MAX_CLIENT_SESSION_LOCK_SURFACES
    );
}
