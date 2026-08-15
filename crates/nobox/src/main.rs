//! Command-line entry point for the nobox window manager.

mod xsmp;

use std::{
    fs::{self, OpenOptions},
    io::{BufRead as _, BufReader, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use nobox_config::{Config, DEFAULT_CONFIG, OpenboxThemeImport, config_path, state_path};
use nobox_runtime::{
    BackendCapabilities, BackendKind, ControlSender, InstanceId, RunDisposition, RunningInstance,
    SessionRestore, SessionSnapshot,
};
use nobox_x11::{WindowManager, X11Diagnostics, running_instance};
use signal_hook::{
    consts::signal::{SIGHUP, SIGINT, SIGTERM},
    iterator::{Handle as SignalHandle, Signals},
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Use a specific configuration file.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Reconnect to an existing XSMP client identity.
    #[arg(long, global = true, hide = true)]
    sm_client_id: Option<String>,

    /// Ask the running nobox instance to exit cleanly.
    #[arg(long, global = true)]
    exit: bool,

    /// Display-server backend. X11 remains the default during Wayland development.
    #[arg(long, global = true, value_enum, default_value_t = Backend::X11)]
    backend: Backend,

    /// X11 display, either as the managed display or nested Wayland host.
    #[arg(long, global = true)]
    display: Option<String>,

    /// Exact opaque runtime instance identity for remote control.
    #[arg(long, global = true)]
    instance: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the selected backend (X11 by default).
    Run {
        /// Do not launch ~/.config/nobox/autostart.
        #[arg(long)]
        no_autostart: bool,

        /// Host the experimental Wayland backend in an X11 window.
        #[arg(long)]
        nested_x11: bool,

        /// Run Wayland directly through libseat/DRM on a dedicated graphical TTY.
        #[arg(long)]
        tty: bool,
    },
    /// Parse and validate the effective configuration.
    Check,
    /// Inspect config, session state, and backend readiness without claiming the display.
    Doctor {
        /// Validate Wayland through nested X11 instead of direct-session prerequisites.
        #[arg(long)]
        nested_x11: bool,
    },
    /// Create a commented configuration file with safe defaults.
    Init {
        /// Replace an existing config.toml.
        #[arg(long)]
        force: bool,
    },
    /// Print the effective config and autostart paths.
    Paths,
    /// Print the built-in default configuration.
    PrintDefault,
    /// Convert representable Openbox 3 themerc properties to validated nobox TOML.
    ImportOpenboxTheme {
        /// Openbox themerc file, theme directory, or openbox-3 directory.
        #[arg(value_name = "PATH")]
        source: PathBuf,

        /// Write the generated minimal config instead of printing it.
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,

        /// Replace an existing output file.
        #[arg(long, requires = "output")]
        force: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Backend {
    X11,
    Wayland,
}

fn main() -> Result<()> {
    init_tracing()?;
    let Cli {
        config,
        sm_client_id,
        exit,
        backend,
        display,
        instance,
        command,
    } = Cli::parse();
    if exit {
        if command.is_some() {
            bail!("--exit cannot be combined with a subcommand");
        }
        let running = match backend {
            Backend::X11 => {
                let running = running_instance(display.as_deref())
                    .context("failed to locate the running X11 nobox instance")?;
                if let Some(requested) = instance.as_deref() {
                    let requested = InstanceId::parse(requested)
                        .context("invalid requested runtime instance")?;
                    if running.id() != &requested {
                        bail!("the active X11 manager does not match --instance {requested}");
                    }
                }
                running
            }
            Backend::Wayland => match instance.as_deref() {
                Some(instance) => RunningInstance::load(
                    BackendKind::Wayland,
                    &InstanceId::parse(instance).context("invalid requested runtime instance")?,
                ),
                None => RunningInstance::discover_unique(BackendKind::Wayland),
            }
            .context("failed to locate one unambiguous Wayland nobox instance")?,
        };
        return running
            .sender()
            .context("failed to validate the running nobox instance")?
            .shutdown()
            .context("failed to request a clean nobox exit");
    }
    if instance.is_some() {
        bail!("--instance is only valid with --exit");
    }
    let path = config.map_or_else(config_path, Ok)?;

    match command.unwrap_or(Command::Run {
        no_autostart: false,
        nested_x11: false,
        tty: false,
    }) {
        Command::Run {
            no_autostart,
            nested_x11,
            tty,
        } => match backend {
            Backend::X11 if nested_x11 || tty => {
                bail!("--nested-x11 and --tty are only valid with --backend wayland")
            }
            Backend::X11 => run_x11(
                &path,
                display.as_deref(),
                no_autostart,
                sm_client_id.as_deref(),
            ),
            Backend::Wayland if tty && display.is_some() => {
                bail!("--display is not valid with the direct Wayland --tty path")
            }
            Backend::Wayland if tty => run_wayland_direct(&path, no_autostart),
            Backend::Wayland if !nested_x11 => {
                bail!("select --nested-x11 for isolated Wayland or --tty for direct libseat/DRM")
            }
            Backend::Wayland => run_wayland_nested(&path, display.as_deref(), no_autostart),
        },
        Command::Check => {
            if path.exists() {
                Config::load(&path)?;
                println!("configuration is valid: {}", path.display());
            } else {
                Config::parse(DEFAULT_CONFIG)?;
                println!(
                    "configuration is valid: using built-in defaults ({} does not exist)",
                    path.display()
                );
            }
            Ok(())
        }
        Command::Doctor { nested_x11 } => match backend {
            Backend::X11 if nested_x11 => {
                bail!("--nested-x11 is only valid with --backend wayland")
            }
            Backend::X11 => doctor(&path, display.as_deref()),
            Backend::Wayland if nested_x11 => doctor_wayland_nested(display.as_deref()),
            Backend::Wayland => doctor_wayland_direct(&path),
        },
        Command::Init { force } => init_config(&path, force),
        Command::Paths => {
            println!("config: {}", path.display());
            println!("autostart: {}", autostart_path(&path).display());
            println!("session: {}", state_path()?.display());
            Ok(())
        }
        Command::PrintDefault => {
            print!("{DEFAULT_CONFIG}");
            Ok(())
        }
        Command::ImportOpenboxTheme {
            source,
            output,
            force,
        } => import_openbox_theme(&source, output.as_deref(), force),
    }
}

fn run_x11(
    path: &Path,
    display: Option<&str>,
    no_autostart: bool,
    requested_sm_client_id: Option<&str>,
) -> Result<()> {
    let session_path = state_path()?;
    let mut restore = load_session_restore(&session_path);
    let mut initial_start = true;
    let mut sm_client_id = requested_sm_client_id.map(str::to_owned);
    let mut panel = PanelSupervisor::new(path, display, BackendCapabilities::X11);

    loop {
        let config = load_or_default(path)?;
        panel.sync(&config);
        let mut wm = WindowManager::connect_with_session(display, config, restore)
            .context("failed to start the X11 backend")?;
        let control = wm
            .start_runtime_control(display)
            .context("failed to create the runtime control endpoint")?;
        let signals = SignalForwarder::install(control.clone())?;
        let xsmp = if xsmp::XsmpBridge::requested() {
            xsmp::XsmpBridge::connect(control, sm_client_id.as_deref())
        } else {
            None
        };
        if initial_start && !no_autostart {
            launch_autostart(path)?;
        }
        initial_start = false;

        let outcome = wm
            .run_with_session_coordination(
                || -> Result<Config> {
                    let config = load_or_default(path)?;
                    panel.sync(&config);
                    Ok(config)
                },
                |snapshot| {
                    let success = match snapshot.save(&session_path) {
                        Ok(()) => true,
                        Err(error) => {
                            warn!(%error, path = %session_path.display(), "could not save requested XSMP snapshot");
                            false
                        }
                    };
                    if let Some(xsmp) = &xsmp {
                        xsmp.save_done(success);
                    }
                    success
                },
                || xsmp.as_ref().is_some_and(xsmp::XsmpBridge::request_logout),
            )
            .context("X11 event loop stopped")?;
        drop(signals);
        let (snapshot, disposition) = outcome.into_parts();
        if let Err(error) = snapshot.save(&session_path) {
            warn!(%error, path = %session_path.display(), "could not save session state");
        }
        if let Some(xsmp) = xsmp {
            sm_client_id = xsmp.client_id().or(sm_client_id);
            let reconnecting = matches!(disposition, RunDisposition::Restart { command: None });
            xsmp.close(!reconnecting);
        }
        panel.stop();
        match disposition {
            RunDisposition::Exit => return Ok(()),
            RunDisposition::Restart { command: None } => {
                info!("restarting nobox without rerunning autostart");
                restore = snapshot.into_restore();
            }
            RunDisposition::Restart {
                command: Some(command),
            } => return replace_with_command(&command),
        }
    }
}

#[cfg(feature = "wayland")]
fn run_wayland_nested(path: &Path, display: Option<&str>, no_autostart: bool) -> Result<()> {
    let session_path = state_path()?;
    let mut restore = load_session_restore(&session_path);
    let mut initial_start = true;
    let mut panel = PanelSupervisor::new(path, display, BackendCapabilities::WAYLAND_NESTED);

    loop {
        let config = load_or_default(path)?;
        panel.sync(&config);
        if initial_start && !no_autostart {
            launch_autostart(path)?;
        }
        initial_start = false;
        let options = nobox_wayland::NestedOptions {
            display: display.map(str::to_owned),
            ..nobox_wayland::NestedOptions::default()
        };
        let report = nobox_wayland::run_nested_with_session(
            options,
            config,
            restore,
            SignalForwarder::install,
            || -> Result<Config> {
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

#[cfg(not(feature = "wayland"))]
fn run_wayland_nested(_path: &Path, _display: Option<&str>, _no_autostart: bool) -> Result<()> {
    bail!("Wayland support was not built; configure with -DNOBOX_BUILD_WAYLAND=ON")
}

#[cfg(feature = "wayland")]
fn run_wayland_direct(path: &Path, no_autostart: bool) -> Result<()> {
    let session_path = state_path()?;
    let mut restore = load_session_restore(&session_path);
    let mut initial_start = true;

    loop {
        let config = load_or_default(path)?;
        let options = nobox_wayland::DirectOptions::default();
        let socket_name = options.socket_name.clone();
        let launch_session = initial_start && !no_autostart;
        initial_start = false;
        let report = nobox_wayland::run_direct_with_session(
            options,
            config,
            restore,
            |control| {
                let signals = SignalForwarder::install(control)?;
                if launch_session {
                    launch_autostart_wayland(path, &socket_name)?;
                }
                Ok::<_, anyhow::Error>(signals)
            },
            || load_or_default(path),
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

#[cfg(not(feature = "wayland"))]
fn run_wayland_direct(_path: &Path, _no_autostart: bool) -> Result<()> {
    bail!("Wayland support was not built; configure with -DNOBOX_BUILD_WAYLAND=ON")
}

struct PanelSupervisor {
    child: Option<Child>,
    config: PathBuf,
    display: Option<String>,
    capabilities: BackendCapabilities,
}

impl PanelSupervisor {
    fn new(config: &Path, display: Option<&str>, capabilities: BackendCapabilities) -> Self {
        Self {
            child: None,
            config: config.to_path_buf(),
            display: display.map(str::to_owned),
            capabilities,
        }
    }

    fn sync(&mut self, config: &Config) {
        if !config.panel.enabled {
            self.stop();
            return;
        }
        if !self.capabilities.panel {
            self.stop();
            warn!(
                backend = %self.capabilities.backend,
                "nobox-panel is not available for the selected backend yet"
            );
            return;
        }
        let executable = std::env::current_exe()
            .ok()
            .map(|mut path| {
                path.set_file_name("nobox-panel");
                path
            })
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("nobox-panel"));
        let mut command = ProcessCommand::new(&executable);
        command
            .arg("--config")
            .arg(&self.config)
            .arg("--ready")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(display) = &self.display {
            command.arg("--display").arg(display);
        }
        match command.spawn() {
            Ok(mut child) => {
                if wait_panel_ready(&mut child) {
                    info!(pid = child.id(), executable = %executable.display(), "started optional panel");
                    let previous = self.child.replace(child);
                    if let Some(previous) = previous {
                        stop_child(previous);
                    }
                } else {
                    warn!(pid = child.id(), executable = %executable.display(), "optional panel did not become ready; retaining previous panel");
                    stop_child(child);
                }
            }
            Err(error) => {
                warn!(%error, executable = %executable.display(), "could not start optional panel");
            }
        }
    }

    fn stop(&mut self) {
        let Some(child) = self.child.take() else {
            return;
        };
        stop_child(child);
    }
}

fn stop_child(mut child: Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn wait_panel_ready(child: &mut Child) -> bool {
    let Some(stdout) = child.stdout.take() else {
        return false;
    };
    let (sender, receiver) = mpsc::channel();
    let reader = thread::Builder::new()
        .name("nobox-panel-ready".to_owned())
        .spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout)
                .read_line(&mut line)
                .map(|read| read > 0 && line.trim() == "ready");
            let _ = sender.send(result.unwrap_or(false));
        });
    if reader.is_err() {
        return false;
    }
    receiver
        .recv_timeout(Duration::from_secs(3))
        .unwrap_or(false)
}

impl Drop for PanelSupervisor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn doctor(path: &Path, display: Option<&str>) -> Result<()> {
    let mut errors = 0_u32;
    let mut warnings = 0_u32;
    let capabilities = BackendCapabilities::X11;
    println!(
        "[ok] backend capabilities: direct={}, session-restore={}, panel={}, agent-seat={}",
        capabilities.direct_session,
        capabilities.session_restore,
        capabilities.panel,
        capabilities.agent_seat
    );
    let config = if path.exists() {
        match Config::load(path) {
            Ok(config) => {
                println!("[ok] config: {}", path.display());
                config
            }
            Err(error) => {
                println!("[error] config: {}: {error}", path.display());
                errors = errors.saturating_add(1);
                Config::default()
            }
        }
    } else {
        println!(
            "[ok] config: built-in defaults ({} does not exist)",
            path.display()
        );
        Config::default()
    };

    let autostart = autostart_path(path);
    if autostart.is_file() {
        println!("[ok] autostart: {}", autostart.display());
    } else if autostart.exists() {
        println!(
            "[warn] autostart is not a regular file: {}",
            autostart.display()
        );
        warnings = warnings.saturating_add(1);
    } else {
        println!("[info] autostart: not installed ({})", autostart.display());
    }

    match state_path() {
        Ok(session) => match SessionSnapshot::load(&session) {
            Ok(_) if session.exists() => println!("[ok] session: {}", session.display()),
            Ok(_) => println!("[info] session: no saved state ({})", session.display()),
            Err(error) => {
                println!("[error] session: {error}");
                errors = errors.saturating_add(1);
            }
        },
        Err(error) => {
            println!("[error] session path: {error}");
            errors = errors.saturating_add(1);
        }
    }

    match X11Diagnostics::inspect(display, &config.theme.font) {
        Ok(diagnostics) => {
            print_x11_diagnostics(&diagnostics, &config.theme.font, &mut errors, &mut warnings);
        }
        Err(error) => {
            let selected = display.unwrap_or("$DISPLAY");
            println!("[error] X11 display {selected}: {error}");
            errors = errors.saturating_add(1);
        }
    }

    if errors == 0 {
        println!("ready: yes ({warnings} warning(s))");
        Ok(())
    } else {
        println!("ready: no ({errors} error(s), {warnings} warning(s))");
        bail!("nobox doctor found {errors} blocking issue(s)")
    }
}

#[cfg(feature = "wayland")]
fn doctor_wayland_nested(display: Option<&str>) -> Result<()> {
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
    println!(
        "[info] surface protocols: wp_viewporter v{}; wp_fractional_scale_manager_v1 v{}",
        nobox_wayland::VIEWPORTER_VERSION,
        nobox_wayland::FRACTIONAL_SCALE_VERSION
    );
    println!(
        "[info] selection protocols: wl_data_device_manager v{}; zwp_primary_selection_device_manager_v1 v{}",
        nobox_wayland::DATA_DEVICE_VERSION,
        nobox_wayland::PRIMARY_SELECTION_VERSION
    );
    print_wayland_selection_limits();
    print_wayland_pointer_protocols();
    print_wayland_presentation_protocol();
    print_wayland_inhibition_protocols();
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

#[cfg(not(feature = "wayland"))]
fn doctor_wayland_nested(_display: Option<&str>) -> Result<()> {
    bail!("Wayland support is not built; configure CMake with -DNOBOX_BUILD_WAYLAND=ON")
}

#[cfg(feature = "wayland")]
fn doctor_wayland_direct(path: &Path) -> Result<()> {
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
    println!(
        "[info] surface protocols: wp_viewporter v{}; wp_fractional_scale_manager_v1 v{}",
        nobox_wayland::VIEWPORTER_VERSION,
        nobox_wayland::FRACTIONAL_SCALE_VERSION
    );
    println!(
        "[info] selection protocols: wl_data_device_manager v{}; zwp_primary_selection_device_manager_v1 v{}",
        nobox_wayland::DATA_DEVICE_VERSION,
        nobox_wayland::PRIMARY_SELECTION_VERSION
    );
    print_wayland_selection_limits();
    print_wayland_pointer_protocols();
    print_wayland_presentation_protocol();
    print_wayland_inhibition_protocols();
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

#[cfg(feature = "wayland")]
fn print_wayland_selection_limits() {
    println!(
        "[info] selection limits per client: {} sources; {} devices; {} MIME types/source; {} bytes/MIME type",
        nobox_wayland::MAX_CLIENT_SELECTION_SOURCES,
        nobox_wayland::MAX_CLIENT_SELECTION_DEVICES,
        nobox_wayland::MAX_SOURCE_MIME_TYPES,
        nobox_wayland::MAX_MIME_TYPE_BYTES
    );
}

#[cfg(feature = "wayland")]
fn print_wayland_pointer_protocols() {
    println!(
        "[info] pointer protocols: zwp_relative_pointer_manager_v{}; zwp_pointer_constraints_v1 v{}; zwp_pointer_gestures_v1 v{}; {} extension objects/client; {} gesture objects/client",
        nobox_wayland::RELATIVE_POINTER_VERSION,
        nobox_wayland::POINTER_CONSTRAINTS_VERSION,
        nobox_wayland::POINTER_GESTURES_VERSION,
        nobox_wayland::MAX_CLIENT_POINTER_EXTENSION_OBJECTS,
        nobox_wayland::MAX_CLIENT_POINTER_GESTURES
    );
}

#[cfg(feature = "wayland")]
fn print_wayland_presentation_protocol() {
    println!(
        "[info] timing protocol: wp_presentation v{}; {} feedbacks/client",
        nobox_wayland::PRESENTATION_VERSION,
        nobox_wayland::MAX_CLIENT_PRESENTATION_FEEDBACKS
    );
}

#[cfg(feature = "wayland")]
fn print_wayland_inhibition_protocols() {
    println!(
        "[info] inhibition protocol: zwp_keyboard_shortcuts_inhibit_manager_v1 v{}; {} inhibitors/client",
        nobox_wayland::KEYBOARD_SHORTCUTS_INHIBIT_VERSION,
        nobox_wayland::MAX_CLIENT_SHORTCUT_INHIBITORS
    );
}

#[cfg(not(feature = "wayland"))]
fn doctor_wayland_direct(_path: &Path) -> Result<()> {
    bail!("Wayland support is not built; configure CMake with -DNOBOX_BUILD_WAYLAND=ON")
}

fn print_x11_diagnostics(
    diagnostics: &X11Diagnostics,
    configured_font: &str,
    errors: &mut u32,
    warnings: &mut u32,
) {
    println!(
        "[ok] X11: {} release {}, protocol {}.{}, screen {} {}x{}x{}",
        diagnostics.vendor,
        diagnostics.release_number,
        diagnostics.protocol_version.0,
        diagnostics.protocol_version.1,
        diagnostics.screen_index,
        diagnostics.width,
        diagnostics.height,
        diagnostics.depth,
    );
    print_extension("RandR", diagnostics.randr_version, warnings);
    print_extension("Shape", diagnostics.shape_version, warnings);
    print_extension("Sync", diagnostics.sync_version, warnings);
    for (index, output) in diagnostics.outputs.iter().enumerate() {
        println!(
            "[ok] output {}: id={} {}x{}{:+}{:+}{}",
            index + 1,
            output.id.raw(),
            output.geometry.width,
            output.geometry.height,
            output.geometry.x,
            output.geometry.y,
            if output.primary { " primary" } else { "" },
        );
    }
    if diagnostics.configured_font_available {
        println!("[ok] X11 font: {configured_font}");
    } else if diagnostics.fallback_font_available {
        println!("[warn] X11 font is unavailable: {configured_font} (startup falls back to fixed)");
        *warnings = warnings.saturating_add(1);
    } else {
        println!("[error] X11 font is unavailable: {configured_font}");
        *errors = errors.saturating_add(1);
    }
    if let Some(owner) = diagnostics.window_manager_owner {
        println!(
            "[warn] another window manager owns this screen: {owner:#x} (exit it before starting nobox)"
        );
        *warnings = warnings.saturating_add(1);
    } else {
        println!("[ok] window-manager selection: available");
    }
}

fn print_extension<T>(name: &str, version: Option<(T, T)>, warnings: &mut u32)
where
    T: std::fmt::Display,
{
    if let Some((major, minor)) = version {
        println!("[ok] X11 extension {name}: {major}.{minor}");
    } else {
        println!("[warn] X11 extension {name}: unavailable; nobox will use its fallback");
        *warnings = warnings.saturating_add(1);
    }
}

fn load_session_restore(path: &Path) -> SessionRestore {
    match SessionSnapshot::load(path) {
        Ok(snapshot) => snapshot.into_restore(),
        Err(error) => {
            warn!(%error, path = %path.display(), "ignoring invalid session state");
            SessionSnapshot::default().into_restore()
        }
    }
}

fn replace_with_command(command: &str) -> Result<()> {
    info!(%command, "replacing nobox after clean backend shutdown");
    let error = ProcessCommand::new("/bin/sh")
        .arg("-c")
        .arg(format!("exec {command}"))
        .exec();
    Err(error).with_context(|| format!("could not replace nobox with `{command}`"))
}

struct SignalForwarder {
    handle: SignalHandle,
    thread: Option<JoinHandle<()>>,
}

impl SignalForwarder {
    fn install(control: ControlSender) -> Result<Self> {
        let mut signals = Signals::new([SIGHUP, SIGINT, SIGTERM])
            .context("could not register runtime signal handlers")?;
        let handle = signals.handle();
        let thread = thread::Builder::new()
            .name("nobox-signals".to_owned())
            .spawn(move || {
                for signal in signals.forever() {
                    let result = match signal {
                        SIGHUP => control.reload(),
                        SIGINT | SIGTERM => control.shutdown(),
                        _ => continue,
                    };
                    if let Err(error) = result {
                        warn!(%error, signal, "could not forward runtime signal");
                        break;
                    }
                    if matches!(signal, SIGINT | SIGTERM) {
                        break;
                    }
                }
            })
            .context("could not start runtime signal thread")?;
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }
}

impl Drop for SignalForwarder {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            warn!("runtime signal thread panicked");
        }
    }
}

fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("nobox=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize logging: {error}"))
}

fn load_or_default(path: &Path) -> Result<Config> {
    if path.exists() {
        return Config::load(path).with_context(|| format!("invalid config: {}", path.display()));
    }
    info!(path = %path.display(), "config not found; using built-in defaults");
    Config::parse(DEFAULT_CONFIG).context("built-in configuration is invalid")
}

fn init_config(path: &Path, force: bool) -> Result<()> {
    let Some(parent) = path.parent() else {
        bail!("configuration path has no parent: {}", path.display());
    };
    fs::create_dir_all(parent).with_context(|| format!("could not create {}", parent.display()))?;

    let mut options = OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = options.open(path).with_context(|| {
        format!(
            "could not create {} (use --force to replace it)",
            path.display()
        )
    })?;
    file.write_all(DEFAULT_CONFIG.as_bytes())
        .with_context(|| format!("could not write {}", path.display()))?;
    println!("created {}", path.display());
    Ok(())
}

fn import_openbox_theme(source: &Path, output: Option<&Path>, force: bool) -> Result<()> {
    let source = resolve_openbox_themerc(source)?;
    let contents = fs::read_to_string(&source)
        .with_context(|| format!("could not read Openbox theme at {}", source.display()))?;
    let imported = OpenboxThemeImport::parse(&contents)
        .with_context(|| format!("could not import Openbox theme at {}", source.display()))?;
    let generated = imported.to_toml();
    eprintln!(
        "mapped {} Openbox properties from {}",
        imported.mapped_properties,
        source.display()
    );
    for warning in &imported.warnings {
        eprintln!("note: {warning}");
    }
    if let Some(output) = output {
        write_imported_theme(output, &generated, force)?;
        println!("created {}", output.display());
    } else {
        print!("{generated}");
    }
    Ok(())
}

fn resolve_openbox_themerc(source: &Path) -> Result<PathBuf> {
    if source.is_file() {
        return Ok(source.to_path_buf());
    }
    if source.is_dir() {
        for candidate in [source.join("openbox-3/themerc"), source.join("themerc")] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        bail!(
            "{} contains neither openbox-3/themerc nor themerc",
            source.display()
        );
    }
    bail!("Openbox theme path does not exist: {}", source.display())
}

fn write_imported_theme(path: &Path, contents: &str, force: bool) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = options.open(path).with_context(|| {
        format!(
            "could not create {} (use --force to replace it)",
            path.display()
        )
    })?;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("could not write {}", path.display()))
}

fn autostart_path(config: &Path) -> PathBuf {
    config.parent().map_or_else(
        || PathBuf::from("autostart"),
        |parent| parent.join("autostart"),
    )
}

fn launch_autostart(config: &Path) -> Result<()> {
    launch_autostart_with(config, |_| {})
}

#[cfg(feature = "wayland")]
fn launch_autostart_wayland(config: &Path, socket_name: &str) -> Result<()> {
    launch_autostart_with(config, |command| {
        command
            .env("WAYLAND_DISPLAY", socket_name)
            .env("XDG_SESSION_TYPE", "wayland")
            .env_remove("DISPLAY");
    })
}

fn launch_autostart_with(config: &Path, configure: impl FnOnce(&mut ProcessCommand)) -> Result<()> {
    let path = autostart_path(config);
    if !path.exists() {
        return Ok(());
    }
    let mut child = ProcessCommand::new("/bin/sh");
    child.arg(&path).stdin(Stdio::null());
    configure(&mut child);
    match child.spawn() {
        Ok(mut process) => {
            let pid = process.id();
            info!(pid, path = %path.display(), "launched autostart");
            let wait_path = path.clone();
            if let Err(error) = thread::Builder::new()
                .name("nobox-autostart".to_owned())
                .spawn(move || match process.wait() {
                    Ok(status) if status.success() => {
                        info!(pid, path = %wait_path.display(), "autostart finished");
                    }
                    Ok(status) => {
                        warn!(pid, %status, path = %wait_path.display(), "autostart exited unsuccessfully");
                    }
                    Err(error) => {
                        warn!(pid, %error, path = %wait_path.display(), "could not wait for autostart");
                    }
                })
            {
                warn!(pid, %error, path = %path.display(), "could not start autostart waiter");
            }
        }
        Err(error) => warn!(%error, path = %path.display(), "could not launch autostart"),
    }
    Ok(())
}
