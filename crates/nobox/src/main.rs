//! Command-line entry point for the nobox X11 window manager.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use nobox_config::{Config, DEFAULT_CONFIG, config_path, state_path};
use nobox_x11::{ControlSender, SessionSnapshot, WindowManager};
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
        } => {
            let config = load_or_default(&path)?;
            let session_path = state_path()?;
            let restore = match SessionSnapshot::load(&session_path) {
                Ok(snapshot) => snapshot.into_restore(),
                Err(error) => {
                    warn!(%error, path = %session_path.display(), "ignoring invalid session state");
                    SessionSnapshot::default().into_restore()
                }
            };
            let wm = WindowManager::connect_with_session(display.as_deref(), config, restore)
                .context("failed to start the X11 backend")?;
            let control = wm
                .control_sender(display.as_deref())
                .context("failed to create the runtime control connection")?;
            let _signals = SignalForwarder::install(control)?;
            if !no_autostart {
                launch_autostart(&path)?;
            }
            let snapshot = wm
                .run(|| load_or_default(&path))
                .context("X11 event loop stopped")?;
            if let Err(error) = snapshot.save(&session_path) {
                warn!(%error, path = %session_path.display(), "could not save session state");
            }
            Ok(())
        }
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
    }
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
