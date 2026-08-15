//! Separate optional standards-based panel process for Nobox.

mod wayland;
mod x11;

use std::{env, path::PathBuf};

use anyhow::Result;
use clap::{Parser, ValueEnum};
use nobox_config::{Config, config_path};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Read a specific Nobox configuration file.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// X11 display, such as :2. Defaults to DISPLAY.
    #[arg(long)]
    display: Option<String>,
    /// Display-server frontend. Auto prefers Wayland when WAYLAND_DISPLAY is set.
    #[arg(long, value_enum, default_value_t = PanelBackend::Auto)]
    backend: PanelBackend,
    /// Notify the session supervisor after the first drawable surface commit.
    #[arg(long, hide = true)]
    ready: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PanelBackend {
    Auto,
    X11,
    Wayland,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.config.map_or_else(config_path, Ok)?;
    let config = if path.exists() {
        Config::load(&path)?
    } else {
        Config::default()
    };
    if !config.panel.enabled {
        return Ok(());
    }
    match cli.backend {
        PanelBackend::Wayland => wayland::run(config, cli.ready),
        PanelBackend::X11 => x11::run(cli.display.as_deref(), config, cli.ready),
        PanelBackend::Auto if env::var_os("WAYLAND_DISPLAY").is_some() => {
            wayland::run(config, cli.ready)
        }
        PanelBackend::Auto => x11::run(cli.display.as_deref(), config, cli.ready),
    }
}
