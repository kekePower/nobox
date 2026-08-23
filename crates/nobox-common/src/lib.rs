//! Backend-neutral command, process, panel, and session support for Nobox.

use std::{
    fs::{self, OpenOptions},
    io::{BufRead as _, BufReader, Write},
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory as _, FromArgMatches as _, Parser, Subcommand, ValueEnum};
use nobox_config::{Config, DEFAULT_CONFIG, OpenboxThemeImport, config_path, state_path};
use nobox_runtime::{
    BackendCapabilities, BackendKind, ControlSender, InstanceId, SessionRestore, SessionSnapshot,
};
use signal_hook::{
    consts::signal::{SIGHUP, SIGINT, SIGTERM},
    iterator::{Handle as SignalHandle, Signals},
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// The backend identity accepted by the compatibility launcher interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Backend {
    /// The X11 window manager.
    X11,
    /// The native Wayland compositor.
    Wayland,
}

#[derive(Debug, Parser)]
#[command(disable_version_flag = true)]
struct Cli {
    /// Use a specific configuration file.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Reconnect to an existing XSMP client identity.
    #[arg(long, global = true, hide = true)]
    sm_client_id: Option<String>,

    /// Ask the running Nobox instance to exit cleanly.
    #[arg(long, global = true)]
    exit: bool,

    /// Assert the display-server backend selected by this executable.
    #[arg(long, global = true, value_enum)]
    backend: Option<Backend>,

    /// X11 display, either as the managed display or nested Wayland host.
    #[arg(long, global = true)]
    display: Option<String>,

    /// Exact opaque runtime instance identity for remote control.
    #[arg(long, global = true)]
    instance: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,

    /// Print version information.
    #[arg(short = 'V', long = "version", action = clap::ArgAction::Version)]
    version: Option<bool>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run this executable's backend.
    Run {
        /// Do not launch ~/.config/nobox/autostart.
        #[arg(long)]
        no_autostart: bool,

        /// Host the Wayland backend in an X11 window.
        #[arg(long)]
        nested_x11: bool,

        /// Run Wayland directly through libseat/DRM on a graphical TTY.
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

        /// Validate direct Wayland prerequisites for a graphical TTY.
        #[arg(long)]
        tty: bool,
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
    /// Convert representable Openbox 3 themerc properties to validated Nobox TOML.
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

/// Backend-specific operations driven by the shared Nobox CLI.
pub trait BackendDriver {
    /// Backend selected by the implementing executable.
    const BACKEND: Backend;
    /// Executable name used in help and diagnostics.
    const BINARY_NAME: &'static str;
    /// Backend crate version.
    const VERSION: &'static str;

    /// Request shutdown of a running backend instance.
    fn exit(display: Option<&str>, instance: Option<&str>) -> Result<()>;

    /// Run the backend until it exits or replaces itself.
    fn run(
        config: &Path,
        display: Option<&str>,
        no_autostart: bool,
        nested_x11: bool,
        tty: bool,
        sm_client_id: Option<&str>,
    ) -> Result<()>;

    /// Diagnose backend readiness without claiming the display or seat.
    fn doctor(config: &Path, display: Option<&str>, nested_x11: bool, tty: bool) -> Result<()>;
}

/// Parse the shared CLI and dispatch it to one concrete backend executable.
pub fn run_backend<Driver: BackendDriver>() -> Result<()> {
    init_tracing()?;
    let matches = Cli::command()
        .name(Driver::BINARY_NAME)
        .version(Driver::VERSION)
        .get_matches();
    let Cli {
        config,
        sm_client_id,
        exit,
        backend,
        display,
        instance,
        command,
        version: _,
    } = Cli::from_arg_matches(&matches)?;

    if let Some(requested) = backend
        && requested != Driver::BACKEND
    {
        bail!(
            "{} cannot run the {} backend",
            Driver::BINARY_NAME,
            requested.as_str()
        );
    }
    if exit {
        if command.is_some() {
            bail!("--exit cannot be combined with a subcommand");
        }
        return Driver::exit(display.as_deref(), instance.as_deref());
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
        } => Driver::run(
            &path,
            display.as_deref(),
            no_autostart,
            nested_x11,
            tty,
            sm_client_id.as_deref(),
        ),
        Command::Doctor { nested_x11, tty } => {
            if nested_x11 && tty {
                bail!("--nested-x11 and --tty are mutually exclusive");
            }
            Driver::doctor(&path, display.as_deref(), nested_x11, tty)
        }
        Command::Check => check_config(&path),
        Command::Init { force } => init_config(&path, force),
        Command::Paths => print_paths(&path),
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

impl Backend {
    fn as_str(self) -> &'static str {
        match self {
            Self::X11 => "X11",
            Self::Wayland => "Wayland",
        }
    }
}

fn check_config(path: &Path) -> Result<()> {
    if path.exists() {
        Config::load(path)?;
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

fn print_paths(path: &Path) -> Result<()> {
    println!("config: {}", path.display());
    println!("autostart: {}", autostart_path(path).display());
    println!("session: {}", state_path()?.display());
    Ok(())
}

/// Supervise the optional panel while preserving readiness handoff on replacement.
pub struct PanelSupervisor {
    child: Arc<Mutex<Option<Child>>>,
    generation: Arc<AtomicU64>,
    config: PathBuf,
    display: Option<String>,
    capabilities: BackendCapabilities,
}

impl PanelSupervisor {
    /// Create a panel supervisor for one backend session.
    #[must_use]
    pub fn new(config: &Path, display: Option<&str>, capabilities: BackendCapabilities) -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
            config: config.to_path_buf(),
            display: display.map(str::to_owned),
            capabilities,
        }
    }

    /// Reconcile the optional panel process with the effective configuration.
    pub fn sync(&self, config: &Config) {
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
        let executable = std::env::var_os("NOBOX_PANEL_EXECUTABLE")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe().ok().map(|mut path| {
                    path.set_file_name("nobox-panel");
                    path
                })
            })
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("nobox-panel"));
        let mut command = ProcessCommand::new(&executable);
        command
            .arg("--config")
            .arg(&self.config)
            .arg("--backend")
            .arg(match self.capabilities.backend {
                BackendKind::X11 => "x11",
                BackendKind::Wayland => "wayland",
            })
            .arg("--ready")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        match (self.capabilities.backend, &self.display) {
            (BackendKind::X11, Some(display)) => {
                command.arg("--display").arg(display);
            }
            (BackendKind::Wayland, Some(display)) => {
                command
                    .env("WAYLAND_DISPLAY", display)
                    .env("XDG_SESSION_TYPE", "wayland")
                    .env_remove("DISPLAY");
            }
            _ => {}
        }
        match command.spawn() {
            Ok(mut candidate) => {
                let pid = candidate.id();
                let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
                let current_generation = Arc::clone(&self.generation);
                let current_child = Arc::clone(&self.child);
                thread::spawn(move || {
                    if wait_panel_ready(&mut candidate)
                        && current_generation.load(Ordering::Acquire) == generation
                    {
                        info!(pid, executable = %executable.display(), "started optional panel");
                        let previous = current_child
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .replace(candidate);
                        if let Some(previous) = previous {
                            stop_child(previous);
                        }
                    } else {
                        warn!(pid, executable = %executable.display(), "optional panel did not become ready; retaining previous panel");
                        stop_child(candidate);
                    }
                });
            }
            Err(error) => {
                warn!(%error, executable = %executable.display(), "could not start optional panel");
            }
        }
    }

    /// Stop the currently active panel, if any.
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        let Some(child) = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return;
        };
        stop_child(child);
    }
}

impl Drop for PanelSupervisor {
    fn drop(&mut self) {
        self.stop();
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

/// Forward POSIX reload and shutdown signals into a backend control endpoint.
pub struct SignalForwarder {
    handle: SignalHandle,
    thread: Option<JoinHandle<()>>,
}

impl SignalForwarder {
    /// Install the session signal forwarding thread.
    pub fn install(control: ControlSender) -> Result<Self> {
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

/// Load the effective configuration or the built-in defaults.
pub fn load_or_default(path: &Path) -> Result<Config> {
    if path.exists() {
        return Config::load(path).with_context(|| format!("invalid config: {}", path.display()));
    }
    info!(path = %path.display(), "config not found; using built-in defaults");
    Config::parse(DEFAULT_CONFIG).context("built-in configuration is invalid")
}

/// Load a saved session, falling back to an empty restore on invalid state.
#[must_use]
pub fn load_session_restore(path: &Path) -> SessionRestore {
    match SessionSnapshot::load(path) {
        Ok(snapshot) => snapshot.into_restore(),
        Err(error) => {
            warn!(%error, path = %path.display(), "ignoring invalid session state");
            SessionSnapshot::default().into_restore()
        }
    }
}

/// Replace the current backend process with a configured shell command.
pub fn replace_with_command(command: &str) -> Result<()> {
    info!(%command, "replacing nobox after clean backend shutdown");
    let error = ProcessCommand::new("/bin/sh")
        .arg("-c")
        .arg(format!("exec {command}"))
        .exec();
    Err(error).with_context(|| format!("could not replace nobox with `{command}`"))
}

/// Launch the Openbox-style autostart script for an X11 session.
pub fn launch_autostart(config: &Path) -> Result<()> {
    launch_autostart_with(config, |_| {})
}

/// Launch autostart with a native Wayland environment and optional XWayland display.
pub fn launch_autostart_wayland(
    config: &Path,
    socket_name: &str,
    xwayland_display: Option<&str>,
) -> Result<()> {
    launch_autostart_with(config, |command| {
        configure_wayland_autostart(command, socket_name, xwayland_display);
    })
}

fn configure_wayland_autostart(
    command: &mut ProcessCommand,
    socket_name: &str,
    xwayland_display: Option<&str>,
) {
    command
        .env("WAYLAND_DISPLAY", socket_name)
        .env("XDG_SESSION_TYPE", "wayland")
        .env_remove("DISPLAY");
    if let Some(display) = xwayland_display {
        command.env("DISPLAY", display);
    }
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

fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("nobox=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize logging: {error}"))
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

/// Parse a backend instance identifier with consistent user-facing context.
pub fn parse_instance(instance: &str) -> Result<InstanceId> {
    InstanceId::parse(instance).context("invalid requested runtime instance")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        ffi::{OsStr, OsString},
    };

    use super::*;

    #[test]
    fn wayland_autostart_receives_native_and_ready_xwayland_displays() {
        let mut command = ProcessCommand::new("true");
        configure_wayland_autostart(&mut command, "nobox-wayland-test", Some(":7"));
        let environment = command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(OsStr::to_owned)))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(
            environment.get(OsStr::new("WAYLAND_DISPLAY")),
            Some(&Some(OsString::from("nobox-wayland-test")))
        );
        assert_eq!(
            environment.get(OsStr::new("XDG_SESSION_TYPE")),
            Some(&Some(OsString::from("wayland")))
        );
        assert_eq!(
            environment.get(OsStr::new("DISPLAY")),
            Some(&Some(OsString::from(":7")))
        );
    }

    #[test]
    fn wayland_autostart_removes_stale_display_without_xwayland() {
        let mut command = ProcessCommand::new("true");
        configure_wayland_autostart(&mut command, "nobox-wayland-test", None);

        assert!(
            command
                .get_envs()
                .any(|(name, value)| { name == OsStr::new("DISPLAY") && value.is_none() })
        );
    }
}
