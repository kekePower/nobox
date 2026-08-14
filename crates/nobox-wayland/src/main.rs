//! Standalone launcher for the experimental Wayland infrastructure proof.

use anyhow::Result;
use clap::Parser;
use nobox_wayland::{NestedOptions, run_nested};
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
    let report = run_nested(options)?;
    tracing::info!(
        frames = report.rendered_frames,
        disconnected_clients = report.disconnected_clients,
        "nested Wayland proof stopped cleanly"
    );
    Ok(())
}
