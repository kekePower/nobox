//! Standalone test launcher for the managed nested Wayland shell.

use anyhow::Result;
use clap::{Parser, ValueEnum};
use nobox_wayland::{NestedOptions, RendererKind, run_nested};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Socket basename to create below XDG_RUNTIME_DIR.
    #[arg(long)]
    socket: Option<String>,

    /// Exit cleanly after this many Wayland clients disconnect.
    #[arg(long, default_value_t = 0)]
    exit_after_disconnects: usize,

    /// Select the nested rendering path.
    #[arg(long, value_enum, default_value_t = Renderer::Auto)]
    renderer: Renderer,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum Renderer {
    #[default]
    Auto,
    Gles2,
    Pixman,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("nobox_wayland=info")),
        )
        .with_target(false)
        .compact()
        .init();
    let cli = Cli::parse();
    let mut options = NestedOptions::default();
    if let Some(socket) = cli.socket {
        options.socket_name = socket;
    }
    options.exit_after_disconnects = cli.exit_after_disconnects;
    options.renderer = match cli.renderer {
        Renderer::Auto => RendererKind::Auto,
        Renderer::Gles2 => RendererKind::Gles2,
        Renderer::Pixman => RendererKind::Pixman,
    };
    let report = run_nested(options)?;
    tracing::info!(
        frames = report.rendered_frames,
        disconnected_clients = report.disconnected_clients,
        renderer = ?report.renderer,
        "nested Wayland shell stopped cleanly"
    );
    Ok(())
}
