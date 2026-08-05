//! Command-line entry point for the nobox X11 window manager.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use nobox_config::{Config, DEFAULT_CONFIG, OpenboxThemeImport, config_path, state_path};
use nobox_x11::{
    ControlSender, RunDisposition, SessionRestore, SessionSnapshot, WindowManager, X11Diagnostics,
};
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

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the X11 window manager (the default command).
    Run {
        /// X11 display, such as :1. Defaults to DISPLAY.
        #[arg(long)]
        display: Option<String>,

        /// Do not launch ~/.config/nobox/autostart.
        #[arg(long)]
        no_autostart: bool,
    },
    /// Parse and validate the effective configuration.
    Check,
    /// Inspect config, session state, and X11 readiness without claiming the display.
    Doctor {
        /// X11 display, such as :1. Defaults to DISPLAY.
        #[arg(long)]
        display: Option<String>,
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

fn main() -> Result<()> {
    init_tracing()?;
    let cli = Cli::parse();
    let path = cli.config.map_or_else(config_path, Ok)?;

    match cli.command.unwrap_or(Command::Run {
        display: None,
        no_autostart: false,
    }) {
        Command::Run {
            display,
            no_autostart,
        } => run_x11(&path, display.as_deref(), no_autostart),
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
        Command::Doctor { display } => doctor(&path, display.as_deref()),
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

fn run_x11(path: &Path, display: Option<&str>, no_autostart: bool) -> Result<()> {
    let session_path = state_path()?;
    let mut restore = load_session_restore(&session_path);
    let mut initial_start = true;

    loop {
        let config = load_or_default(path)?;
        let wm = WindowManager::connect_with_session(display, config, restore)
            .context("failed to start the X11 backend")?;
        let control = wm
            .control_sender(display)
            .context("failed to create the runtime control connection")?;
        let signals = SignalForwarder::install(control)?;
        if initial_start && !no_autostart {
            launch_autostart(path)?;
        }
        initial_start = false;

        let outcome = wm
            .run(|| load_or_default(path))
            .context("X11 event loop stopped")?;
        drop(signals);
        let (snapshot, disposition) = outcome.into_parts();
        if let Err(error) = snapshot.save(&session_path) {
            warn!(%error, path = %session_path.display(), "could not save session state");
        }
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

fn doctor(path: &Path, display: Option<&str>) -> Result<()> {
    let mut errors = 0_u32;
    let mut warnings = 0_u32;
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
    info!(%command, "replacing nobox after clean X11 shutdown");
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
    let path = autostart_path(config);
    if !path.exists() {
        return Ok(());
    }
    let mut child = ProcessCommand::new("/bin/sh");
    child
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match child.spawn() {
        Ok(process) => info!(pid = process.id(), path = %path.display(), "launched autostart"),
        Err(error) => warn!(%error, path = %path.display(), "could not launch autostart"),
    }
    Ok(())
}
