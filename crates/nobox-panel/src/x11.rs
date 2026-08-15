//! X11/EWMH frontend for the optional panel.

use std::{
    env,
    io::{self, Write as _},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::Local;
use nobox_config::{Config, PanelItem, PanelPosition, PanelTaskScope};
use nobox_desktop::{ApplicationCatalog, DesktopApplication};
use x11rb::{
    COPY_DEPTH_FROM_PARENT,
    connection::Connection,
    errors::ReplyError,
    protocol::{
        ErrorKind, Event,
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
        WM_CHANGE_STATE,
        WM_CLASS,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST,
        _NET_CLOSE_WINDOW,
        _NET_CURRENT_DESKTOP,
        _NET_DESKTOP_NAMES,
        _NET_NUMBER_OF_DESKTOPS,
        _NET_WM_DESKTOP,
        _NET_WM_NAME,
        _NET_WM_STATE,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_DEMANDS_ATTENTION,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_STICKY,
        _NET_WM_STRUT,
        _NET_WM_STRUT_PARTIAL,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NOBOX_PANEL_CLOCK,
        _NOBOX_PANEL_LAUNCHER_COUNT,
        _NOBOX_PANEL_TASK_COUNT,
        _NOBOX_PANEL_WORKSPACE_COUNT,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PanelAction {
    Workspace(u32),
    Task { window: Window, active: bool },
    Launcher(usize),
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
    urgent: bool,
    iconified: bool,
}

#[derive(Clone, Copy)]
enum ButtonStyle {
    Normal,
    Active,
    Urgent,
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
    launchers: Vec<DesktopApplication>,
    drawn: Option<PanelContent>,
}

pub(super) fn run(display: Option<&str>, config: Config, ready: bool) -> Result<()> {
    let panel = Panel::new(display, config)?;
    if ready {
        println!("ready");
        io::stdout().flush()?;
    }
    panel.run()
}

impl Panel {
    fn new(display: Option<&str>, config: Config) -> Result<Self> {
        let catalog = ApplicationCatalog::discover();
        let launchers = config
            .panel
            .launchers
            .iter()
            .filter_map(|desktop_id| catalog.find(desktop_id).cloned())
            .collect();
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
                .foreground(config.panel.foreground.pixel())
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
            launchers,
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
                        self.activate_at(event.event_x, event.detail, event.time)?;
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
        let clock = Local::now()
            .format(&self.config.panel.clock_format)
            .to_string();
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

        self.draw_items(current, count, names, tasks, clock)?;

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
            self.atoms._NOBOX_PANEL_LAUNCHER_COUNT,
            AtomEnum::CARDINAL,
            &[u32::try_from(self.launchers.len()).unwrap_or(u32::MAX)],
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
            let desktop = optional_client_reply(desktop.reply())?;
            let states = optional_client_reply(states.reply())?;
            let types = optional_client_reply(types.reply())?;
            let name = optional_client_reply(name.reply())?;
            let fallback_name = optional_client_reply(fallback_name.reply())?;
            let (Some(desktop), Some(states), Some(types), Some(name), Some(fallback_name)) =
                (desktop, states, types, name, fallback_name)
            else {
                continue;
            };
            let desktop = desktop
                .value32()
                .and_then(|mut values| values.next())
                .unwrap_or(current);
            if self.config.panel.task_scope == PanelTaskScope::CurrentWorkspace
                && desktop != current
                && desktop != u32::MAX
            {
                continue;
            }
            let states = states.value32().map_or_else(Vec::new, Iterator::collect);
            if states.contains(&self.atoms._NET_WM_STATE_SKIP_TASKBAR) {
                continue;
            }
            if types.value32().is_some_and(|mut atoms| {
                atoms.any(|atom| atom == self.atoms._NET_WM_WINDOW_TYPE_DOCK)
            }) {
                continue;
            }
            let title = text_from_bytes(name.value)
                .or_else(|| text_from_bytes(fallback_name.value))
                .unwrap_or_else(|| format!("{window:#x}"));
            tasks.push(Task {
                window,
                title,
                active: active == Some(window),
                urgent: states.contains(&self.atoms._NET_WM_STATE_DEMANDS_ATTENTION),
                iconified: states.contains(&self.atoms._NET_WM_STATE_HIDDEN),
            });
        }
        Ok(tasks)
    }

    fn draw_items(
        &mut self,
        current: u32,
        count: u32,
        names: &[String],
        tasks: &[Task],
        clock: &str,
    ) -> Result<()> {
        let items = self.visible_items(tasks);
        let padding = i16::try_from(self.config.panel.padding).unwrap_or(i16::MAX);
        let spacing = i16::try_from(self.config.panel.spacing).unwrap_or(i16::MAX);
        let gaps = i16::try_from(items.len().saturating_sub(1)).unwrap_or(i16::MAX);
        let fixed = items.iter().fold(0_i16, |width, item| {
            width.saturating_add(self.fixed_item_width(*item, count, names, clock))
        });
        let content_width = i16::try_from(self.width)
            .unwrap_or(i16::MAX)
            .saturating_sub(padding.saturating_mul(2))
            .saturating_sub(spacing.saturating_mul(gaps));
        let flexible = content_width.saturating_sub(fixed).max(0);
        let has_spacer = items.contains(&PanelItem::Spacer);
        let tasks_width = if items.contains(&PanelItem::Tasks) {
            if has_spacer {
                flexible.min(
                    i16::try_from(tasks.len())
                        .unwrap_or(i16::MAX)
                        .saturating_mul(
                            i16::try_from(self.config.panel.task_max_width).unwrap_or(i16::MAX),
                        ),
                )
            } else {
                flexible
            }
        } else {
            0
        };
        let spacer_width = flexible.saturating_sub(tasks_width);
        let mut x = padding;
        for (position, item) in items.iter().enumerate() {
            match item {
                PanelItem::Launchers => {
                    for index in 0..self.launchers.len() {
                        let label = self.launchers[index].name.clone();
                        let width = self.text_button_width(&label, 28, 140);
                        self.draw_button(
                            x,
                            width,
                            &label,
                            ButtonStyle::Normal,
                            PanelAction::Launcher(index),
                        )?;
                        x = x.saturating_add(width);
                        if index + 1 < self.launchers.len() {
                            x = x.saturating_add(spacing);
                        }
                    }
                }
                PanelItem::Workspaces => {
                    for index in 0..count {
                        let fallback;
                        let label = match names.get(usize::try_from(index).unwrap_or(usize::MAX)) {
                            Some(name) => name.as_str(),
                            None => {
                                fallback = (index + 1).to_string();
                                &fallback
                            }
                        };
                        let width = self.text_button_width(label, 28, 120);
                        let style = if index == current {
                            ButtonStyle::Active
                        } else {
                            ButtonStyle::Normal
                        };
                        self.draw_button(x, width, label, style, PanelAction::Workspace(index))?;
                        x = x.saturating_add(width);
                        if index + 1 < count {
                            x = x.saturating_add(spacing);
                        }
                    }
                }
                PanelItem::Tasks => {
                    let button_room = tasks_width.saturating_sub(spacing.saturating_mul(
                        i16::try_from(tasks.len().saturating_sub(1)).unwrap_or(i16::MAX),
                    ));
                    let each = if tasks.is_empty() {
                        0
                    } else {
                        button_room / i16::try_from(tasks.len()).unwrap_or(i16::MAX).max(1)
                    };
                    for (index, task) in tasks.iter().enumerate() {
                        if each <= 0 {
                            break;
                        }
                        let style = if task.urgent {
                            ButtonStyle::Urgent
                        } else if task.active {
                            ButtonStyle::Active
                        } else {
                            ButtonStyle::Normal
                        };
                        let label = if task.iconified {
                            format!("[{}]", task.title)
                        } else {
                            task.title.clone()
                        };
                        self.draw_button(
                            x,
                            each,
                            &label,
                            style,
                            PanelAction::Task {
                                window: task.window,
                                active: task.active,
                            },
                        )?;
                        x = x.saturating_add(each);
                        if index + 1 < tasks.len() {
                            x = x.saturating_add(spacing);
                        }
                    }
                }
                PanelItem::Spacer => x = x.saturating_add(spacer_width),
                PanelItem::Clock => {
                    let width = self.text_button_width(clock, 48, 192);
                    self.draw_label(x, width, clock, ButtonStyle::Normal)?;
                    x = x.saturating_add(width);
                }
            }
            if position + 1 < items.len() {
                x = x.saturating_add(spacing);
            }
        }
        Ok(())
    }

    fn visible_items(&self, tasks: &[Task]) -> Vec<PanelItem> {
        self.config
            .panel
            .items
            .iter()
            .copied()
            .filter(|item| match item {
                PanelItem::Launchers => !self.launchers.is_empty(),
                PanelItem::Workspaces => self.config.panel.show_workspaces,
                PanelItem::Tasks => self.config.panel.show_tasks && !tasks.is_empty(),
                PanelItem::Spacer => true,
                PanelItem::Clock => self.config.panel.show_clock,
            })
            .collect()
    }

    fn fixed_item_width(&self, item: PanelItem, count: u32, names: &[String], clock: &str) -> i16 {
        let spacing = i16::try_from(self.config.panel.spacing).unwrap_or(i16::MAX);
        match item {
            PanelItem::Launchers => self
                .launchers
                .iter()
                .map(|launcher| self.text_button_width(&launcher.name, 28, 140))
                .fold(0_i16, i16::saturating_add)
                .saturating_add(spacing.saturating_mul(
                    i16::try_from(self.launchers.len().saturating_sub(1)).unwrap_or(i16::MAX),
                )),
            PanelItem::Workspaces => (0..count)
                .map(|index| {
                    names
                        .get(usize::try_from(index).unwrap_or(usize::MAX))
                        .map_or_else(
                            || self.text_button_width(&(index + 1).to_string(), 28, 120),
                            |name| self.text_button_width(name, 28, 120),
                        )
                })
                .fold(0_i16, i16::saturating_add)
                .saturating_add(
                    spacing
                        .saturating_mul(i16::try_from(count.saturating_sub(1)).unwrap_or(i16::MAX)),
                ),
            PanelItem::Clock => self.text_button_width(clock, 48, 192),
            PanelItem::Tasks | PanelItem::Spacer => 0,
        }
    }

    fn draw_button(
        &mut self,
        x: i16,
        width: i16,
        label: &str,
        style: ButtonStyle,
        action: PanelAction,
    ) -> Result<()> {
        self.draw_label(x, width, label, style)?;
        self.targets.push(HitTarget {
            left: x,
            right: x.saturating_add(width),
            action,
        });
        Ok(())
    }

    fn draw_label(&self, x: i16, width: i16, label: &str, style: ButtonStyle) -> Result<()> {
        let color = match style {
            ButtonStyle::Normal => self.config.panel.background.pixel(),
            ButtonStyle::Active => self.config.panel.active_background.pixel(),
            ButtonStyle::Urgent => self.config.panel.urgent_background.pixel(),
        };
        self.set_gc_foreground(color)?;
        let vertical_padding = u16::try_from(self.config.panel.padding)
            .unwrap_or(u16::MAX)
            .min(self.height.saturating_sub(1) / 2);
        self.connection.poly_fill_rectangle(
            self.window,
            self.gc,
            &[Rectangle {
                x,
                y: i16::try_from(vertical_padding).unwrap_or(i16::MAX),
                width: u16::try_from(width.max(1)).unwrap_or(1),
                height: self
                    .height
                    .saturating_sub(vertical_padding.saturating_mul(2)),
            }],
        )?;
        let text = fit_core_text(label, width.saturating_sub(12), self.character_width);
        let baseline = i16::try_from(self.height / 2)
            .unwrap_or(i16::MAX)
            .saturating_add(self.font_ascent / 2);
        self.set_gc_foreground(self.config.panel.foreground.pixel())?;
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

    fn activate_at(&self, x: i16, button: u8, timestamp: u32) -> Result<()> {
        let Some(target) = self
            .targets
            .iter()
            .find(|target| x >= target.left && x < target.right)
        else {
            return Ok(());
        };
        let (window, atom, data) = match target.action {
            PanelAction::Workspace(index) if button == 1 => (
                self.root,
                self.atoms._NET_CURRENT_DESKTOP,
                [index, timestamp, 0, 0, 0],
            ),
            PanelAction::Task { window, active } if button == 1 && active => {
                (window, self.atoms.WM_CHANGE_STATE, [3, timestamp, 0, 0, 0])
            }
            PanelAction::Task { window, .. } if button == 1 => (
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
            PanelAction::Task { window, .. } if button == 3 => (
                window,
                self.atoms._NET_CLOSE_WINDOW,
                [timestamp, 2, 0, 0, 0],
            ),
            PanelAction::Task { .. } if button == 4 || button == 5 => {
                return self.cycle_tasks(button == 5, timestamp);
            }
            PanelAction::Launcher(index) if button == 1 => {
                self.launch(index);
                return Ok(());
            }
            _ => return Ok(()),
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

    fn cycle_tasks(&self, forward: bool, timestamp: u32) -> Result<()> {
        let Some(content) = self
            .drawn
            .as_ref()
            .filter(|content| !content.tasks.is_empty())
        else {
            return Ok(());
        };
        let current = content.tasks.iter().position(|task| task.active);
        let index = match (current, forward) {
            (Some(index), true) => (index + 1) % content.tasks.len(),
            (Some(0), false) | (None, false) => content.tasks.len() - 1,
            (Some(index), false) => index - 1,
            (None, true) => 0,
        };
        let window = content.tasks[index].window;
        let message = ClientMessageEvent::new(
            32,
            window,
            self.atoms._NET_ACTIVE_WINDOW,
            [2, timestamp, 0, 0, 0],
        );
        self.connection.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            message,
        )?;
        self.connection.flush()?;
        Ok(())
    }

    fn launch(&self, index: usize) {
        let Some(application) = self.launchers.get(index) else {
            return;
        };
        let Some((program, arguments)) = application.command.argv().split_first() else {
            return;
        };
        let mut process = if application.command.requires_terminal() {
            let terminal = env::var_os("TERMINAL")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "xterm".into());
            let mut process = Command::new(terminal);
            process.arg("-e").arg(program).args(arguments);
            process
        } else {
            let mut process = Command::new(program);
            process.args(arguments);
            process
        };
        if let Some(directory) = application.command.working_directory() {
            process.current_dir(directory);
        }
        process
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match process.spawn() {
            Ok(mut child) => {
                let name = format!("panel-launch-{}", child.id());
                let _ = thread::Builder::new().name(name).spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(error) => eprintln!(
                "nobox-panel: could not launch {}: {error}",
                application.desktop_id
            ),
        }
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

fn optional_client_reply<T>(reply: std::result::Result<T, ReplyError>) -> Result<Option<T>> {
    match reply {
        Ok(reply) => Ok(Some(reply)),
        Err(ReplyError::X11Error(error)) if error.error_kind == ErrorKind::Window => Ok(None),
        Err(error) => Err(error.into()),
    }
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
