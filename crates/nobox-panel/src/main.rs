//! Separate optional EWMH panel process for nobox.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use nobox_config::{Config, PanelPosition, config_path};
use x11rb::{
    COPY_DEPTH_FROM_PARENT,
    connection::Connection,
    protocol::{
        Event,
        xproto::{AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, WindowClass},
    },
    wrapper::ConnectionExt as _,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        UTF8_STRING,
        WM_CLASS,
        _NET_WM_NAME,
        _NET_WM_DESKTOP,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_STATE,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_STICKY,
        _NET_WM_STRUT,
        _NET_WM_STRUT_PARTIAL,
    }
}

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Read a specific nobox configuration file.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// X11 display, such as :2. Defaults to DISPLAY.
    #[arg(long)]
    display: Option<String>,
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
    run_panel(cli.display.as_deref(), config)
}

fn run_panel(display: Option<&str>, config: Config) -> Result<()> {
    let (connection, screen_index) = x11rb::connect(display).context("connect to X11")?;
    let screen = connection
        .setup()
        .roots
        .get(screen_index)
        .context("invalid X11 screen")?;
    let root = screen.root;
    let width = u32::from(screen.width_in_pixels);
    let root_height = u32::from(screen.height_in_pixels);
    let height = config.panel.height.min(root_height).max(1);
    let y = match config.panel.position {
        PanelPosition::Top => 0,
        PanelPosition::Bottom => {
            i32::try_from(root_height.saturating_sub(height)).unwrap_or(i32::MAX)
        }
    };
    let window = connection.generate_id()?;
    connection.create_window(
        COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        i16::try_from(y).unwrap_or(i16::MAX),
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(height).unwrap_or(u16::MAX),
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(config.panel.background.pixel())
            .event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY),
    )?;
    let atoms = Atoms::new(&connection)?.reply()?;
    connection.change_property32(
        PropMode::REPLACE,
        window,
        atoms._NET_WM_WINDOW_TYPE,
        AtomEnum::ATOM,
        &[atoms._NET_WM_WINDOW_TYPE_DOCK],
    )?;
    connection.change_property32(
        PropMode::REPLACE,
        window,
        atoms._NET_WM_STATE,
        AtomEnum::ATOM,
        &[
            atoms._NET_WM_STATE_ABOVE,
            atoms._NET_WM_STATE_STICKY,
            atoms._NET_WM_STATE_SKIP_TASKBAR,
            atoms._NET_WM_STATE_SKIP_PAGER,
        ],
    )?;
    connection.change_property32(
        PropMode::REPLACE,
        window,
        atoms._NET_WM_DESKTOP,
        AtomEnum::CARDINAL,
        &[u32::MAX],
    )?;
    let (strut, partial) = panel_strut(config.panel.position, width, height);
    connection.change_property32(
        PropMode::REPLACE,
        window,
        atoms._NET_WM_STRUT,
        AtomEnum::CARDINAL,
        &strut,
    )?;
    connection.change_property32(
        PropMode::REPLACE,
        window,
        atoms._NET_WM_STRUT_PARTIAL,
        AtomEnum::CARDINAL,
        &partial,
    )?;
    connection.change_property8(
        PropMode::REPLACE,
        window,
        atoms._NET_WM_NAME,
        atoms.UTF8_STRING,
        b"nobox panel",
    )?;
    connection.change_property8(
        PropMode::REPLACE,
        window,
        atoms.WM_CLASS,
        AtomEnum::STRING,
        b"nobox-panel\0Nobox-panel\0",
    )?;
    connection.map_window(window)?;
    connection.flush()?;

    loop {
        match connection.wait_for_event()? {
            Event::DestroyNotify(event) if event.window == window => return Ok(()),
            _ => {}
        }
    }
}

fn panel_strut(position: PanelPosition, width: u32, height: u32) -> ([u32; 4], [u32; 12]) {
    let end = width.saturating_sub(1);
    let mut strut = [0_u32; 4];
    let mut partial = [0_u32; 12];
    match position {
        PanelPosition::Top => {
            strut[2] = height;
            partial[2] = height;
            partial[8] = 0;
            partial[9] = end;
        }
        PanelPosition::Bottom => {
            strut[3] = height;
            partial[3] = height;
            partial[10] = 0;
            partial[11] = end;
        }
    }
    (strut, partial)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_panel_reserves_only_its_horizontal_span() {
        let (strut, partial) = panel_strut(PanelPosition::Bottom, 800, 30);
        assert_eq!(strut, [0, 0, 0, 30]);
        assert_eq!(partial[3], 30);
        assert_eq!(&partial[10..=11], &[0, 799]);
    }
}
