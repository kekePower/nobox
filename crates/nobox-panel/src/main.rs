//! Separate optional EWMH panel process for nobox.

use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::Local;
use clap::Parser;
use nobox_config::{Config, PanelPosition, config_path};
use x11rb::{
    COPY_DEPTH_FROM_PARENT,
    connection::Connection,
    protocol::{
        Event,
        xproto::{
            Atom, AtomEnum, ChangeGCAux, ChangeWindowAttributesAux, ClientMessageEvent,
            ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, Font, Gcontext, PropMode,
            Rectangle, Window, WindowClass,
        },
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        UTF8_STRING,
        WM_CLASS,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST,
        _NET_CURRENT_DESKTOP,
        _NET_DESKTOP_NAMES,
        _NET_NUMBER_OF_DESKTOPS,
        _NET_WM_DESKTOP,
        _NET_WM_NAME,
        _NET_WM_STATE,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_STICKY,
        _NET_WM_STRUT,
        _NET_WM_STRUT_PARTIAL,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NOBOX_PANEL_CLOCK,
        _NOBOX_PANEL_TASK_COUNT,
        _NOBOX_PANEL_WORKSPACE_COUNT,
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum PanelAction {
    Workspace(u32),
    Activate(Window),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HitTarget {
    left: i16,
    right: i16,
    action: PanelAction,
}

#[derive(Debug, Eq, PartialEq)]
struct Task {
    window: Window,
    title: String,
    active: bool,
}

/// Everything a repaint depends on; repainting identical content is skipped.
#[derive(Debug, Eq, PartialEq)]
struct PanelContent {
    current: u32,
    count: u32,
    names: Vec<String>,
    tasks: Vec<Task>,
    clock: String,
    width: u16,
    height: u16,
}

struct Panel {
    connection: RustConnection,
    root: Window,
    window: Window,
    gc: Gcontext,
    font: Font,
    atoms: Atoms,
    config: Config,
    width: u16,
    height: u16,
    font_ascent: i16,
    character_width: u16,
    targets: Vec<HitTarget>,
    drawn: Option<PanelContent>,
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
    Panel::new(cli.display.as_deref(), config)?.run()
}

impl Panel {
    fn new(display: Option<&str>, config: Config) -> Result<Self> {
        let (connection, screen_index) = x11rb::connect(display).context("connect to X11")?;
        let screen = connection
            .setup()
            .roots
            .get(screen_index)
            .context("invalid X11 screen")?;
        let root = screen.root;
        let width = screen.width_in_pixels;
        let root_height = u32::from(screen.height_in_pixels);
        let height = u16::try_from(config.panel.height.min(root_height).max(1))
            .unwrap_or(screen.height_in_pixels);
        let y = match config.panel.position {
            PanelPosition::Top => 0,
            PanelPosition::Bottom => screen.height_in_pixels.saturating_sub(height),
        };
        let window = connection.generate_id()?;
        connection.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            i16::try_from(y).unwrap_or(i16::MAX),
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(config.panel.background.pixel())
                .event_mask(
                    EventMask::BUTTON_PRESS | EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY,
                ),
        )?;
        connection.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        let atoms = Atoms::new(&connection)?.reply()?;
        set_window_properties(
            &connection,
            &atoms,
            window,
            config.panel.position,
            u32::from(width),
            u32::from(height),
        )?;

        let font = open_font(&connection, &config.theme.font)?;
        let font_info = connection.query_font(font)?.reply()?;
        let font_ascent = font_info.font_ascent;
        let character_width = font_info.max_bounds.character_width.unsigned_abs().max(1);
        let gc = connection.generate_id()?;
        connection.create_gc(
            gc,
            window,
            &CreateGCAux::new()
                .foreground(config.theme.title_text.pixel())
                .background(config.panel.background.pixel())
                .font(font)
                .graphics_exposures(0),
        )?;
        connection.map_window(window)?;
        connection.flush()?;

        Ok(Self {
            connection,
            root,
            window,
            gc,
            font,
            atoms,
            config,
            width,
            height,
            font_ascent,
            character_width,
            targets: Vec::new(),
            drawn: None,
        })
    }

    fn run(mut self) -> Result<()> {
        let mut redraw = true;
        let mut repaint = true;
        let mut last_redraw = Instant::now();
        loop {
            while let Some(event) = self.connection.poll_for_event()? {
                match event {
                    Event::DestroyNotify(event) if event.window == self.window => return Ok(()),
                    Event::ConfigureNotify(event) if event.window == self.window => {
                        self.width = event.width;
                        self.height = event.height;
                        redraw = true;
                        repaint = true;
                    }
                    Event::ButtonPress(event) if event.event == self.window => {
                        self.activate_at(event.event_x, event.time)?;
                    }
                    Event::Expose(event) if event.window == self.window => {
                        redraw = true;
                        repaint = true;
                    }
                    Event::PropertyNotify(event) if event.window == self.root => redraw = true,
                    _ => {}
                }
            }
            if redraw {
                self.draw(repaint)?;
                redraw = false;
                repaint = false;
                last_redraw = Instant::now();
            }
            thread::sleep(Duration::from_millis(200));
            if last_redraw.elapsed() >= Duration::from_secs(1) {
                redraw = true;
            }
        }
    }

    fn draw(&mut self, repaint: bool) -> Result<()> {
        let current = read_cardinal(&self.connection, self.root, self.atoms._NET_CURRENT_DESKTOP)?
            .unwrap_or(0);
        let count = read_cardinal(
            &self.connection,
            self.root,
            self.atoms._NET_NUMBER_OF_DESKTOPS,
        )?
        .unwrap_or(1)
        .clamp(1, 256);
        let names = read_desktop_names(&self.connection, self.root, &self.atoms, count)?;
        let tasks = if self.config.panel.show_tasks {
            self.read_tasks(current)?
        } else {
            Vec::new()
        };
        let clock = Local::now().format("%H:%M").to_string();
        let content = PanelContent {
            current,
            count,
            names,
            tasks,
            clock,
            width: self.width,
            height: self.height,
        };
        if !repaint && self.drawn.as_ref() == Some(&content) {
            return Ok(());
        }
        let PanelContent {
            current,
            count,
            ref names,
            ref tasks,
            ref clock,
            ..
        } = content;

        self.set_gc_foreground(self.config.panel.background.pixel())?;
        self.connection.poly_fill_rectangle(
            self.window,
            self.gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
            }],
        )?;
        self.targets.clear();

        let mut leading = 4_i16;
        if self.config.panel.show_workspaces {
            for index in 0..count {
                let fallback;
                let label = match names.get(usize::try_from(index).unwrap_or(usize::MAX)) {
                    Some(name) => name.as_str(),
                    None => {
                        fallback = (index + 1).to_string();
                        &fallback
                    }
                };
                let button_width = self.text_button_width(label, 28, 120);
                self.draw_button(
                    leading,
                    button_width,
                    label,
                    index == current,
                    PanelAction::Workspace(index),
                )?;
                leading = leading.saturating_add(button_width).saturating_add(4);
            }
        }

        let trailing = if self.config.panel.show_clock {
            let clock_width = self.text_button_width(clock, 48, 96);
            let x = i16::try_from(self.width)
                .unwrap_or(i16::MAX)
                .saturating_sub(clock_width)
                .saturating_sub(4);
            self.draw_label(x, clock_width, clock, false)?;
            x.saturating_sub(4)
        } else {
            i16::try_from(self.width)
                .unwrap_or(i16::MAX)
                .saturating_sub(4)
        };

        if !tasks.is_empty() && trailing > leading {
            let available = u16::try_from(trailing - leading).unwrap_or(0);
            let task_width =
                (available / u16::try_from(tasks.len()).unwrap_or(u16::MAX)).clamp(1, 220);
            for task in tasks {
                if leading.saturating_add(i16::try_from(task_width).unwrap_or(i16::MAX)) > trailing
                {
                    break;
                }
                self.draw_button(
                    leading,
                    i16::try_from(task_width).unwrap_or(i16::MAX),
                    &task.title,
                    task.active,
                    PanelAction::Activate(task.window),
                )?;
                leading = leading
                    .saturating_add(i16::try_from(task_width).unwrap_or(i16::MAX))
                    .saturating_add(4);
            }
        }

        self.connection.change_property32(
            PropMode::REPLACE,
            self.window,
            self.atoms._NOBOX_PANEL_WORKSPACE_COUNT,
            AtomEnum::CARDINAL,
            &[count],
        )?;
        self.connection.change_property32(
            PropMode::REPLACE,
            self.window,
            self.atoms._NOBOX_PANEL_TASK_COUNT,
            AtomEnum::CARDINAL,
            &[u32::try_from(tasks.len()).unwrap_or(u32::MAX)],
        )?;
        self.connection.change_property8(
            PropMode::REPLACE,
            self.window,
            self.atoms._NOBOX_PANEL_CLOCK,
            self.atoms.UTF8_STRING,
            clock.as_bytes(),
        )?;
        self.connection.flush()?;
        self.drawn = Some(content);
        Ok(())
    }

    fn read_tasks(&self, current: u32) -> Result<Vec<Task>> {
        let active = read_window(&self.connection, self.root, self.atoms._NET_ACTIVE_WINDOW)?;
        let clients = read_windows(&self.connection, self.root, self.atoms._NET_CLIENT_LIST)?;
        // Issue every per-window request first so the replies arrive in one
        // pipelined batch instead of one blocking round trip per property.
        let mut pending = Vec::with_capacity(clients.len().min(4096));
        for window in clients.into_iter().take(4096) {
            let desktop = self.connection.get_property(
                false,
                window,
                self.atoms._NET_WM_DESKTOP,
                AtomEnum::CARDINAL,
                0,
                1,
            )?;
            let states = self.connection.get_property(
                false,
                window,
                self.atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                0,
                256,
            )?;
            let types = self.connection.get_property(
                false,
                window,
                self.atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                0,
                256,
            )?;
            let name = self.connection.get_property(
                false,
                window,
                self.atoms._NET_WM_NAME,
                self.atoms.UTF8_STRING,
                0,
                1024,
            )?;
            let fallback_name = self.connection.get_property(
                false,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::ANY,
                0,
                1024,
            )?;
            pending.push((window, desktop, states, types, name, fallback_name));
        }
        let mut tasks = Vec::new();
        for (window, desktop, states, types, name, fallback_name) in pending {
            let desktop = desktop
                .reply()?
                .value32()
                .and_then(|mut values| values.next())
                .unwrap_or(current);
            if desktop != current && desktop != u32::MAX {
                continue;
            }
            if states.reply()?.value32().is_some_and(|mut atoms| {
                atoms.any(|atom| atom == self.atoms._NET_WM_STATE_SKIP_TASKBAR)
            }) {
                continue;
            }
            if types.reply()?.value32().is_some_and(|mut atoms| {
                atoms.any(|atom| atom == self.atoms._NET_WM_WINDOW_TYPE_DOCK)
            }) {
                continue;
            }
            let title = text_from_bytes(name.reply()?.value)
                .or_else(|| {
                    fallback_name
                        .reply()
                        .ok()
                        .and_then(|reply| text_from_bytes(reply.value))
                })
                .unwrap_or_else(|| format!("{window:#x}"));
            tasks.push(Task {
                window,
                title,
                active: active == Some(window),
            });
        }
        Ok(tasks)
    }

    fn draw_button(
        &mut self,
        x: i16,
        width: i16,
        label: &str,
        active: bool,
        action: PanelAction,
    ) -> Result<()> {
        self.draw_label(x, width, label, active)?;
        self.targets.push(HitTarget {
            left: x,
            right: x.saturating_add(width),
            action,
        });
        Ok(())
    }

    fn draw_label(&self, x: i16, width: i16, label: &str, active: bool) -> Result<()> {
        let color = if active {
            self.config.theme.active_titlebar.pixel()
        } else {
            self.config.theme.inactive_titlebar.pixel()
        };
        self.set_gc_foreground(color)?;
        self.connection.poly_fill_rectangle(
            self.window,
            self.gc,
            &[Rectangle {
                x,
                y: 3,
                width: u16::try_from(width.max(1)).unwrap_or(1),
                height: self.height.saturating_sub(6),
            }],
        )?;
        let text = fit_core_text(label, width.saturating_sub(12), self.character_width);
        let baseline = i16::try_from(self.height / 2)
            .unwrap_or(i16::MAX)
            .saturating_add(self.font_ascent / 2);
        self.set_gc_foreground(self.config.theme.title_text.pixel())?;
        self.connection
            .image_text8(self.window, self.gc, x.saturating_add(6), baseline, &text)?;
        Ok(())
    }

    fn text_button_width(&self, label: &str, minimum: i16, maximum: i16) -> i16 {
        let characters = label.chars().count().min(255);
        i16::try_from(characters)
            .unwrap_or(i16::MAX)
            .saturating_mul(i16::try_from(self.character_width).unwrap_or(i16::MAX))
            .saturating_add(12)
            .clamp(minimum, maximum)
    }

    fn set_gc_foreground(&self, pixel: u32) -> Result<()> {
        self.connection
            .change_gc(self.gc, &ChangeGCAux::new().foreground(pixel))?;
        Ok(())
    }

    fn activate_at(&self, x: i16, timestamp: u32) -> Result<()> {
        let Some(target) = self
            .targets
            .iter()
            .find(|target| x >= target.left && x < target.right)
        else {
            return Ok(());
        };
        let (window, atom, data) = match target.action {
            PanelAction::Workspace(index) => (
                self.root,
                self.atoms._NET_CURRENT_DESKTOP,
                [index, timestamp, 0, 0, 0],
            ),
            PanelAction::Activate(window) => (
                window,
                self.atoms._NET_ACTIVE_WINDOW,
                [
                    2,
                    timestamp,
                    read_window(&self.connection, self.root, self.atoms._NET_ACTIVE_WINDOW)?
                        .unwrap_or(0),
                    0,
                    0,
                ],
            ),
        };
        let message = ClientMessageEvent::new(32, window, atom, data);
        self.connection.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            message,
        )?;
        self.connection.flush()?;
        Ok(())
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        let _ = self.connection.free_gc(self.gc);
        let _ = self.connection.close_font(self.font);
    }
}

fn set_window_properties(
    connection: &RustConnection,
    atoms: &Atoms,
    window: Window,
    position: PanelPosition,
    width: u32,
    height: u32,
) -> Result<()> {
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
    let (strut, partial) = panel_strut(position, width, height);
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
    Ok(())
}

fn open_font(connection: &RustConnection, requested: &str) -> Result<Font> {
    let font = connection.generate_id()?;
    if connection
        .open_font(font, requested.as_bytes())?
        .check()
        .is_ok()
    {
        return Ok(font);
    }
    let fallback = connection.generate_id()?;
    connection
        .open_font(fallback, b"fixed")?
        .check()
        .context("open configured panel font and fallback 'fixed'")?;
    Ok(fallback)
}

fn read_cardinal(connection: &RustConnection, window: Window, atom: Atom) -> Result<Option<u32>> {
    Ok(connection
        .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 1)?
        .reply()?
        .value32()
        .and_then(|mut values| values.next()))
}

fn read_window(connection: &RustConnection, window: Window, atom: Atom) -> Result<Option<Window>> {
    Ok(connection
        .get_property(false, window, atom, AtomEnum::WINDOW, 0, 1)?
        .reply()?
        .value32()
        .and_then(|mut values| values.next()))
}

fn read_windows(connection: &RustConnection, window: Window, atom: Atom) -> Result<Vec<Window>> {
    Ok(connection
        .get_property(false, window, atom, AtomEnum::WINDOW, 0, 4096)?
        .reply()?
        .value32()
        .map_or_else(Vec::new, Iterator::collect))
}

fn read_text(
    connection: &RustConnection,
    window: Window,
    atom: Atom,
    property_type: Atom,
) -> Result<Option<String>> {
    let reply = connection
        .get_property(false, window, atom, property_type, 0, 1024)?
        .reply()?;
    Ok(text_from_bytes(reply.value))
}

fn text_from_bytes(value: Vec<u8>) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    // Valid UTF-8, the overwhelmingly common case, reuses the reply buffer.
    Some(match String::from_utf8(value) {
        Ok(text) => text,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    })
}

fn read_desktop_names(
    connection: &RustConnection,
    root: Window,
    atoms: &Atoms,
    count: u32,
) -> Result<Vec<String>> {
    let source = read_text(
        connection,
        root,
        atoms._NET_DESKTOP_NAMES,
        atoms.UTF8_STRING,
    )?
    .unwrap_or_default();
    let mut names = source
        .split('\0')
        .take(usize::try_from(count).unwrap_or(usize::MAX))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for index in names.len()..usize::try_from(count).unwrap_or(usize::MAX) {
        names.push((index + 1).to_string());
    }
    Ok(names)
}

fn fit_core_text(label: &str, width: i16, character_width: u16) -> Vec<u8> {
    let capacity = usize::try_from(width.max(0)).unwrap_or(0) / usize::from(character_width.max(1));
    label
        .chars()
        .map(|character| {
            if !character.is_control() && u32::from(character) <= u32::from(u8::MAX) {
                u8::try_from(character).unwrap_or(b'?')
            } else {
                b'?'
            }
        })
        .take(capacity.min(255))
        .collect()
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

    #[test]
    fn core_text_is_bounded_and_sanitized() {
        assert_eq!(fit_core_text("héλlo", 24, 6), b"h\xe9?l");
        assert!(fit_core_text(&"x".repeat(300), 10_000, 1).len() <= 255);
    }
}
