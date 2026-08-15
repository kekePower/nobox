//! Native Wayland layer-shell frontend for the optional panel.

use std::{
    env,
    fs::{File, OpenOptions, remove_file},
    io::{Seek as _, SeekFrom, Write as _},
    os::fd::AsFd as _,
    os::unix::fs::OpenOptionsExt as _,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use chrono::Local;
use fontdb::{Database, Family, Query};
use fontdue::{Font, FontSettings};
use nobox_config::{Config, PanelItem, PanelPosition, PanelTaskScope, RgbColor};
use nobox_desktop::{ApplicationCatalog, DesktopApplication};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
    protocol::{
        wl_buffer, wl_compositor, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
        wl_surface,
    },
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
    },
    workspace::v1::client::{
        ext_workspace_group_handle_v1, ext_workspace_handle_v1, ext_workspace_manager_v1,
    },
};
use wayland_protocols_wlr::{
    foreign_toplevel::v1::client::{
        zwlr_foreign_toplevel_handle_v1, zwlr_foreign_toplevel_manager_v1,
    },
    layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1},
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const MAX_BACKING_BUFFERS: usize = 4;
const MAX_PANEL_DIMENSION: u32 = 16_384;
const MAX_PANEL_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_TASKS: usize = 4096;
const MAX_WORKSPACES: usize = 256;

#[derive(Clone)]
enum PanelAction {
    Workspace(ext_workspace_handle_v1::ExtWorkspaceHandleV1),
    Task {
        handle: zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
        active: bool,
    },
    Launcher(usize),
}

struct HitTarget {
    left: i32,
    right: i32,
    action: PanelAction,
}

struct Workspace {
    handle: ext_workspace_handle_v1::ExtWorkspaceHandleV1,
    name: String,
    active: bool,
}

struct Task {
    handle: zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
    title: String,
    app_id: String,
    active: bool,
    iconified: bool,
    outputs: Vec<wl_output::WlOutput>,
}

struct BackingBuffer {
    id: u64,
    _buffer: wl_buffer::WlBuffer,
    _file: File,
}

#[derive(Clone, Copy)]
struct BufferData {
    id: u64,
}

struct TextRenderer {
    font: Font,
}

impl TextRenderer {
    fn load(configured: &str) -> Result<Self> {
        let mut database = Database::new();
        database.load_system_fonts();
        let requested = configured
            .split_once('-')
            .map_or(configured, |(_, family)| family)
            .trim();
        let families = [
            Family::Name(requested),
            Family::Name("DejaVu Sans"),
            Family::Name("Liberation Sans"),
            Family::SansSerif,
        ];
        let id = database
            .query(&Query {
                families: &families,
                ..Query::default()
            })
            .context("no usable panel font is installed")?;
        let font = database
            .with_face_data(id, |data, collection_index| {
                Font::from_bytes(
                    data,
                    FontSettings {
                        collection_index,
                        ..FontSettings::default()
                    },
                )
            })
            .context("panel font disappeared while loading")?
            .map_err(|_| anyhow::anyhow!("could not parse the selected panel font"))?;
        Ok(Self { font })
    }

    fn measure(&self, text: &str, pixels: u16) -> i32 {
        let pixels = f32::from(pixels.max(1));
        let mut width = 0.0_f32;
        let mut previous = None;
        for character in text.chars() {
            if let Some(previous) = previous {
                width += self
                    .font
                    .horizontal_kern(previous, character, pixels)
                    .unwrap_or(0.0);
            }
            width += self.font.metrics(character, pixels).advance_width;
            previous = Some(character);
        }
        width.ceil().clamp(0.0, i32::MAX as f32) as i32
    }

    fn draw(
        &self,
        pixels: &mut [u32],
        stride: usize,
        bounds: (i32, i32, i32, i32),
        text: &str,
        color: u32,
        em: u16,
    ) {
        let (left, top, width, height) = bounds;
        if width <= 0 || height <= 0 {
            return;
        }
        let em_f32 = f32::from(em.max(1));
        let line = self.font.horizontal_line_metrics(em_f32);
        let line_height = line.map_or(em_f32, |metrics| metrics.ascent - metrics.descent);
        let ascent = line.map_or(em_f32 * 0.8, |metrics| metrics.ascent);
        let baseline = f64::from(top)
            + (f64::from(height) - f64::from(line_height)).max(0.0) / 2.0
            + f64::from(ascent);
        let right = left.saturating_add(width);
        let bottom = top.saturating_add(height);
        let mut pen = f64::from(left);
        let mut previous = None;
        for character in text.chars() {
            if let Some(previous) = previous {
                pen += f64::from(
                    self.font
                        .horizontal_kern(previous, character, em_f32)
                        .unwrap_or(0.0),
                );
            }
            let (metrics, bitmap) = self.font.rasterize(character, em_f32);
            let glyph_left = (pen.round() as i32).saturating_add(metrics.xmin);
            let glyph_top = (baseline.round() as i32)
                .saturating_sub(i32::try_from(metrics.height).unwrap_or(i32::MAX))
                .saturating_sub(metrics.ymin);
            for row in 0..metrics.height {
                let y = glyph_top.saturating_add(i32::try_from(row).unwrap_or(i32::MAX));
                if y < top || y >= bottom {
                    continue;
                }
                for column in 0..metrics.width {
                    let x = glyph_left.saturating_add(i32::try_from(column).unwrap_or(i32::MAX));
                    if x < left || x >= right {
                        continue;
                    }
                    let coverage = bitmap[row * metrics.width + column];
                    if coverage == 0 {
                        continue;
                    }
                    let Some(index) = usize::try_from(y)
                        .ok()
                        .and_then(|y| y.checked_mul(stride))
                        .and_then(|index| {
                            usize::try_from(x).ok().and_then(|x| index.checked_add(x))
                        })
                    else {
                        continue;
                    };
                    if let Some(pixel) = pixels.get_mut(index) {
                        *pixel = blend(*pixel, color, coverage);
                    }
                }
            }
            pen += f64::from(metrics.advance_width);
            previous = Some(character);
            if pen >= f64::from(right) {
                break;
            }
        }
    }
}

struct Panel {
    config: Config,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    outputs: Vec<wl_output::WlOutput>,
    seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    foreign_list: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    workspace_manager: Option<ext_workspace_manager_v1::ExtWorkspaceManagerV1>,
    task_manager: Option<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1>,
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    workspaces: Vec<Workspace>,
    tasks: Vec<Task>,
    launchers: Vec<DesktopApplication>,
    targets: Vec<HitTarget>,
    buffers: Vec<BackingBuffer>,
    next_buffer_id: u64,
    width: u32,
    height: u32,
    pointer_x: f64,
    axis_value120: i32,
    axis_discrete: i32,
    text: TextRenderer,
    content_key: String,
    dirty: bool,
    closed: bool,
    fatal_error: Option<String>,
    ready_requested: bool,
}

pub(super) fn run(config: Config, ready: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let connection = loop {
        match Connection::connect_to_env() {
            Ok(connection) => break connection,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error).context("connect to Wayland"),
        }
    };
    let mut event_queue = connection.new_event_queue();
    let queue = event_queue.handle();
    connection.display().get_registry(&queue, ());
    let catalog = ApplicationCatalog::discover();
    let launchers = config
        .panel
        .launchers
        .iter()
        .filter_map(|desktop_id| catalog.find(desktop_id).cloned())
        .collect();
    let text = TextRenderer::load(&config.theme.font)?;
    let mut panel = Panel {
        config,
        compositor: None,
        shm: None,
        outputs: Vec::new(),
        seat: None,
        pointer: None,
        layer_shell: None,
        foreign_list: None,
        workspace_manager: None,
        task_manager: None,
        surface: None,
        layer_surface: None,
        workspaces: Vec::new(),
        tasks: Vec::new(),
        launchers,
        targets: Vec::new(),
        buffers: Vec::new(),
        next_buffer_id: 1,
        width: 0,
        height: 0,
        pointer_x: 0.0,
        axis_value120: 0,
        axis_discrete: 0,
        text,
        content_key: String::new(),
        dirty: true,
        closed: false,
        fatal_error: None,
        ready_requested: ready,
    };
    event_queue.roundtrip(&mut panel)?;
    panel.initialize(&queue)?;
    event_queue.roundtrip(&mut panel)?;
    panel.require_protocols()?;

    while !panel.closed {
        event_queue.dispatch_pending(&mut panel)?;
        panel.repaint(&queue)?;
        connection.flush()?;
        let Some(read_guard) = event_queue.prepare_read() else {
            continue;
        };
        let mut descriptors = [PollFd::new(&connection, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: 1,
            tv_nsec: 0,
        };
        if poll(&mut descriptors, Some(&timeout))? > 0
            && descriptors[0].revents().contains(PollFlags::IN)
        {
            read_guard.read()?;
        }
    }
    if let Some(error) = panel.fatal_error {
        bail!(error);
    }
    Ok(())
}

impl Panel {
    fn initialize(&mut self, queue: &QueueHandle<Self>) -> Result<()> {
        let compositor = self
            .compositor
            .as_ref()
            .context("wl_compositor is not advertised")?;
        let shell = self
            .layer_shell
            .as_ref()
            .context("zwlr_layer_shell_v1 is not advertised")?;
        let surface = compositor.create_surface(queue, ());
        let layer = shell.get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Top,
            "nobox-panel".to_owned(),
            queue,
            (),
        );
        let height = self.config.panel.height.max(1);
        layer.set_size(0, height);
        let anchor = match self.config.panel.position {
            PanelPosition::Top => zwlr_layer_surface_v1::Anchor::Top,
            PanelPosition::Bottom => zwlr_layer_surface_v1::Anchor::Bottom,
        } | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right;
        layer.set_anchor(anchor);
        layer.set_exclusive_zone(i32::try_from(height).unwrap_or(i32::MAX));
        layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        surface.commit();
        self.surface = Some(surface);
        self.layer_surface = Some(layer);
        Ok(())
    }

    fn require_protocols(&self) -> Result<()> {
        ensure!(self.shm.is_some(), "wl_shm is not advertised");
        ensure!(!self.outputs.is_empty(), "wl_output is not advertised");
        ensure!(self.seat.is_some(), "wl_seat is not advertised");
        ensure!(
            self.foreign_list.is_some(),
            "ext_foreign_toplevel_list_v1 is not advertised"
        );
        ensure!(
            self.workspace_manager.is_some(),
            "ext_workspace_manager_v1 is not advertised"
        );
        ensure!(
            self.task_manager.is_some(),
            "zwlr_foreign_toplevel_manager_v1 is not advertised"
        );
        Ok(())
    }

    fn repaint(&mut self, queue: &QueueHandle<Self>) -> Result<()> {
        if self.width == 0 || self.height == 0 || self.buffers.len() >= MAX_BACKING_BUFFERS {
            return Ok(());
        }
        let clock = Local::now()
            .format(&self.config.panel.clock_format)
            .to_string();
        let key = format!(
            "{}x{}:{:?}:{:?}:{}",
            self.width,
            self.height,
            self.workspaces
                .iter()
                .map(|workspace| (&workspace.name, workspace.active))
                .collect::<Vec<_>>(),
            self.tasks
                .iter()
                .map(|task| (&task.title, task.active, task.iconified, task.outputs.len()))
                .collect::<Vec<_>>(),
            clock
        );
        if !self.dirty && key == self.content_key {
            return Ok(());
        }
        let width = usize::try_from(self.width)?;
        let height = usize::try_from(self.height)?;
        let length = width
            .checked_mul(height)
            .context("panel dimensions overflow")?;
        let mut pixels = vec![argb(self.config.panel.background); length];
        self.targets.clear();
        self.draw_items(&mut pixels, width, &clock);
        let bytes = pixels
            .iter()
            .flat_map(|pixel| pixel.to_ne_bytes())
            .collect::<Vec<_>>();
        let (mut file, path) = create_backing_file(bytes.len(), self.next_buffer_id)?;
        file.write_all(&bytes)?;
        file.seek(SeekFrom::Start(0))?;
        let shm = self.shm.as_ref().expect("validated before repaint");
        let length_i32 = i32::try_from(bytes.len()).context("panel buffer is too large")?;
        let pool = shm.create_pool(file.as_fd(), length_i32, queue, ());
        let id = self.next_buffer_id;
        self.next_buffer_id = self.next_buffer_id.saturating_add(1);
        let stride = i32::try_from(self.width.checked_mul(4).context("panel stride overflow")?)?;
        let buffer = pool.create_buffer(
            0,
            i32::try_from(self.width)?,
            i32::try_from(self.height)?,
            stride,
            wl_shm::Format::Argb8888,
            queue,
            BufferData { id },
        );
        pool.destroy();
        let _ = remove_file(path);
        let surface = self.surface.as_ref().expect("initialized before repaint");
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(
            0,
            0,
            i32::try_from(self.width)?,
            i32::try_from(self.height)?,
        );
        surface.commit();
        self.buffers.push(BackingBuffer {
            id,
            _buffer: buffer,
            _file: file,
        });
        self.content_key = key;
        self.dirty = false;
        if self.ready_requested {
            println!("ready");
            std::io::stdout().flush()?;
            self.ready_requested = false;
        }
        Ok(())
    }

    fn draw_items(&mut self, pixels: &mut [u32], stride: usize, clock: &str) {
        let tasks = self
            .tasks
            .iter()
            .filter(|task| {
                self.config.panel.task_scope == PanelTaskScope::AllWorkspaces
                    || !task.outputs.is_empty()
            })
            .take(MAX_TASKS)
            .map(|task| {
                (
                    task.handle.clone(),
                    task.title.clone(),
                    task.active,
                    task.iconified,
                )
            })
            .collect::<Vec<_>>();
        let items = self
            .config
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
            .collect::<Vec<_>>();
        let spacing = i32::try_from(self.config.panel.spacing).unwrap_or(i32::MAX);
        let padding = i32::try_from(self.config.panel.padding).unwrap_or(i32::MAX);
        let fixed = items.iter().fold(0_i32, |total, item| {
            total.saturating_add(self.fixed_width(*item, clock))
        });
        let content_width =
            i32::try_from(self.width)
                .unwrap_or(i32::MAX)
                .saturating_sub(padding.saturating_mul(2))
                .saturating_sub(spacing.saturating_mul(
                    i32::try_from(items.len().saturating_sub(1)).unwrap_or(i32::MAX),
                ));
        let flexible = content_width.saturating_sub(fixed).max(0);
        let has_spacer = items.contains(&PanelItem::Spacer);
        let tasks_width = if items.contains(&PanelItem::Tasks) {
            if has_spacer {
                flexible.min(
                    i32::try_from(tasks.len())
                        .unwrap_or(i32::MAX)
                        .saturating_mul(
                            i32::try_from(self.config.panel.task_max_width).unwrap_or(i32::MAX),
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
                        let width = self.button_width(&label, 28, 140);
                        self.draw_button(
                            pixels,
                            stride,
                            x,
                            width,
                            &label,
                            self.config.panel.background,
                            PanelAction::Launcher(index),
                        );
                        x = x.saturating_add(width).saturating_add(spacing);
                    }
                    x = x.saturating_sub(spacing);
                }
                PanelItem::Workspaces => {
                    let workspace_items = self
                        .workspaces
                        .iter()
                        .map(|workspace| {
                            (
                                workspace.handle.clone(),
                                workspace.name.clone(),
                                workspace.active,
                            )
                        })
                        .collect::<Vec<_>>();
                    for (handle, label, active) in workspace_items {
                        let width = self.button_width(&label, 28, 120);
                        let color = if active {
                            self.config.panel.active_background
                        } else {
                            self.config.panel.background
                        };
                        self.draw_button(
                            pixels,
                            stride,
                            x,
                            width,
                            &label,
                            color,
                            PanelAction::Workspace(handle),
                        );
                        x = x.saturating_add(width).saturating_add(spacing);
                    }
                    x = x.saturating_sub(spacing);
                }
                PanelItem::Tasks => {
                    let room = tasks_width.saturating_sub(spacing.saturating_mul(
                        i32::try_from(tasks.len().saturating_sub(1)).unwrap_or(i32::MAX),
                    ));
                    let each = room
                        .checked_div(i32::try_from(tasks.len()).unwrap_or(i32::MAX).max(1))
                        .unwrap_or(0);
                    for (handle, title, active, iconified) in tasks.iter().cloned() {
                        if each <= 0 {
                            break;
                        }
                        let label = if iconified {
                            format!("[{title}]")
                        } else {
                            title
                        };
                        let color = if active {
                            self.config.panel.active_background
                        } else {
                            self.config.panel.background
                        };
                        self.draw_button(
                            pixels,
                            stride,
                            x,
                            each,
                            &label,
                            color,
                            PanelAction::Task { handle, active },
                        );
                        x = x.saturating_add(each).saturating_add(spacing);
                    }
                    x = x.saturating_sub(spacing);
                }
                PanelItem::Spacer => x = x.saturating_add(spacer_width),
                PanelItem::Clock => {
                    let width = self.button_width(clock, 48, 192);
                    self.draw_label(
                        pixels,
                        stride,
                        x,
                        width,
                        clock,
                        self.config.panel.background,
                    );
                    x = x.saturating_add(width);
                }
            }
            if position + 1 < items.len() {
                x = x.saturating_add(spacing);
            }
        }
    }

    fn fixed_width(&self, item: PanelItem, clock: &str) -> i32 {
        let spacing = i32::try_from(self.config.panel.spacing).unwrap_or(i32::MAX);
        match item {
            PanelItem::Launchers => summed_widths(
                self.launchers
                    .iter()
                    .map(|launcher| self.button_width(&launcher.name, 28, 140)),
                self.launchers.len(),
                spacing,
            ),
            PanelItem::Workspaces => summed_widths(
                self.workspaces
                    .iter()
                    .map(|workspace| self.button_width(&workspace.name, 28, 120)),
                self.workspaces.len(),
                spacing,
            ),
            PanelItem::Clock => self.button_width(clock, 48, 192),
            PanelItem::Tasks | PanelItem::Spacer => 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_button(
        &mut self,
        pixels: &mut [u32],
        stride: usize,
        x: i32,
        width: i32,
        label: &str,
        color: RgbColor,
        action: PanelAction,
    ) {
        self.draw_label(pixels, stride, x, width, label, color);
        self.targets.push(HitTarget {
            left: x,
            right: x.saturating_add(width),
            action,
        });
    }

    fn draw_label(
        &self,
        pixels: &mut [u32],
        stride: usize,
        x: i32,
        width: i32,
        label: &str,
        color: RgbColor,
    ) {
        let padding = i32::try_from(self.config.panel.padding)
            .unwrap_or(i32::MAX)
            .min(i32::try_from(self.height / 2).unwrap_or(i32::MAX));
        fill_rect(
            pixels,
            stride,
            i32::try_from(self.height).unwrap_or(i32::MAX),
            (
                x,
                padding,
                width.max(1),
                i32::try_from(self.height)
                    .unwrap_or(i32::MAX)
                    .saturating_sub(padding.saturating_mul(2)),
            ),
            argb(color),
        );
        let em = u16::try_from(self.height.saturating_mul(3).saturating_div(5).clamp(8, 24))
            .unwrap_or(12);
        self.text.draw(
            pixels,
            stride,
            (
                x.saturating_add(6),
                0,
                width.saturating_sub(12),
                i32::try_from(self.height).unwrap_or(i32::MAX),
            ),
            label,
            argb(self.config.panel.foreground),
            em,
        );
    }

    fn button_width(&self, label: &str, minimum: i32, maximum: i32) -> i32 {
        let em = u16::try_from(self.height.saturating_mul(3).saturating_div(5).clamp(8, 24))
            .unwrap_or(12);
        self.text
            .measure(label, em)
            .saturating_add(12)
            .clamp(minimum, maximum)
    }

    fn activate_at(&mut self, button: u32) {
        let x = self
            .pointer_x
            .floor()
            .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
        let Some(action) = self
            .targets
            .iter()
            .find(|target| x >= target.left && x < target.right)
            .map(|target| target.action.clone())
        else {
            return;
        };
        match action {
            PanelAction::Workspace(handle) if button == BTN_LEFT => {
                handle.activate();
                if let Some(manager) = &self.workspace_manager {
                    manager.commit();
                }
            }
            PanelAction::Task { handle, active } if button == BTN_LEFT && active => {
                handle.set_minimized();
            }
            PanelAction::Task { handle, .. } if button == BTN_LEFT => {
                handle.unset_minimized();
                if let Some(seat) = &self.seat {
                    handle.activate(seat);
                }
            }
            PanelAction::Task { handle, .. } if button == BTN_RIGHT => handle.close(),
            PanelAction::Launcher(index) if button == BTN_LEFT => self.launch(index),
            _ => {}
        }
    }

    fn cycle_tasks(&self, forward: bool) {
        let visible = self
            .tasks
            .iter()
            .filter(|task| {
                self.config.panel.task_scope == PanelTaskScope::AllWorkspaces
                    || !task.outputs.is_empty()
            })
            .collect::<Vec<_>>();
        if visible.is_empty() {
            return;
        }
        let current = visible.iter().position(|task| task.active);
        let index = match (current, forward) {
            (Some(index), true) => (index + 1) % visible.len(),
            (Some(0), false) | (None, false) => visible.len() - 1,
            (Some(index), false) => index - 1,
            (None, true) => 0,
        };
        if let Some(seat) = &self.seat {
            visible[index].handle.unset_minimized();
            visible[index].handle.activate(seat);
        }
    }

    fn launch(&self, index: usize) {
        let Some(application) = self.launchers.get(index) else {
            return;
        };
        let Some((program, arguments)) = application.command.argv().split_first() else {
            return;
        };
        let mut command = if application.command.requires_terminal() {
            let terminal = env::var_os("TERMINAL")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "xterm".into());
            let mut command = Command::new(terminal);
            command.arg("-e").arg(program).args(arguments);
            command
        } else {
            let mut command = Command::new(program);
            command.args(arguments);
            command
        };
        if let Some(directory) = application.command.working_directory() {
            command.current_dir(directory);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match command.spawn() {
            Ok(mut child) => {
                let _ = thread::Builder::new()
                    .name(format!("panel-launch-{}", child.id()))
                    .spawn(move || {
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

fn create_backing_file(length: usize, id: u64) -> Result<(File, PathBuf)> {
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .context("XDG_RUNTIME_DIR is required for a Wayland panel")?;
    for attempt in 0..16_u32 {
        let path = runtime.join(format!(
            ".nobox-panel-{}-{id}-{attempt}",
            std::process::id()
        ));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .mode(0o600)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                file.set_len(u64::try_from(length)?)?;
                return Ok((file, path));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("could not allocate a unique panel SHM file")
}

fn argb(color: RgbColor) -> u32 {
    0xff00_0000 | color.pixel()
}

fn blend(background: u32, foreground: u32, coverage: u8) -> u32 {
    let alpha = u32::from(coverage);
    let inverse = 255_u32.saturating_sub(alpha);
    let channel = |shift: u32| -> u32 {
        (((foreground >> shift) & 0xff_u32) * alpha + ((background >> shift) & 0xff_u32) * inverse)
            / 255_u32
    };
    0xff00_0000 | (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

fn fill_rect(
    pixels: &mut [u32],
    stride: usize,
    surface_height: i32,
    (x, y, width, height): (i32, i32, i32, i32),
    color: u32,
) {
    let left = x.max(0);
    let top = y.max(0);
    let right = x
        .saturating_add(width)
        .min(i32::try_from(stride).unwrap_or(i32::MAX));
    let bottom = y.saturating_add(height).min(surface_height);
    for row in top..bottom {
        let Some(start) = usize::try_from(row)
            .ok()
            .and_then(|row| row.checked_mul(stride))
            .and_then(|index| {
                usize::try_from(left)
                    .ok()
                    .and_then(|left| index.checked_add(left))
            })
        else {
            continue;
        };
        let Some(end) = usize::try_from(right.saturating_sub(left))
            .ok()
            .and_then(|width| start.checked_add(width))
        else {
            continue;
        };
        if let Some(row) = pixels.get_mut(start..end) {
            row.fill(color);
        }
    }
}

fn summed_widths(widths: impl Iterator<Item = i32>, count: usize, spacing: i32) -> i32 {
    widths.fold(0_i32, i32::saturating_add).saturating_add(
        spacing.saturating_mul(i32::try_from(count.saturating_sub(1)).unwrap_or(i32::MAX)),
    )
}

impl Dispatch<wl_registry::WlRegistry, ()> for Panel {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => {
                state.compositor = Some(registry.bind(name, version.min(6), queue, ()))
            }
            "wl_shm" => state.shm = Some(registry.bind(name, version.min(1), queue, ())),
            "wl_output" => state
                .outputs
                .push(registry.bind(name, version.min(4), queue, ())),
            "wl_seat" => state.seat = Some(registry.bind(name, version.min(9), queue, ())),
            "zwlr_layer_shell_v1" => {
                state.layer_shell = Some(registry.bind(name, version.min(4), queue, ()))
            }
            "ext_foreign_toplevel_list_v1" => {
                state.foreign_list = Some(registry.bind(name, version.min(1), queue, ()))
            }
            "ext_workspace_manager_v1" => {
                state.workspace_manager = Some(registry.bind(name, version.min(1), queue, ()))
            }
            "zwlr_foreign_toplevel_manager_v1" => {
                state.task_manager = Some(registry.bind(name, version.min(3), queue, ()))
            }
            _ => {}
        }
    }
}

delegate_noop!(Panel: ignore wl_compositor::WlCompositor);
delegate_noop!(Panel: ignore wl_shm::WlShm);
delegate_noop!(Panel: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Panel: ignore wl_output::WlOutput);
delegate_noop!(Panel: ignore wl_surface::WlSurface);
delegate_noop!(Panel: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);

impl Dispatch<wl_buffer::WlBuffer, BufferData> for Panel {
    fn event(
        state: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        data: &BufferData,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_buffer::Event::Release) {
            buffer.destroy();
            state.buffers.retain(|backing| backing.id != data.id);
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Panel {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(queue, ()));
            } else if !capabilities.contains(wl_seat::Capability::Pointer) {
                if let Some(pointer) = state.pointer.take() {
                    pointer.release();
                }
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Panel {
    fn event(
        state: &mut Self,
        _pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { surface_x, .. }
            | wl_pointer::Event::Motion { surface_x, .. } => state.pointer_x = surface_x,
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } => state.activate_at(button),
            wl_pointer::Event::AxisValue120 { value120, .. } => {
                state.axis_value120 = state.axis_value120.saturating_add(value120);
            }
            wl_pointer::Event::AxisDiscrete { discrete, .. } => {
                state.axis_discrete = state.axis_discrete.saturating_add(discrete);
            }
            wl_pointer::Event::Frame => {
                let delta = if state.axis_value120 != 0 {
                    state.axis_value120
                } else {
                    state.axis_discrete
                };
                state.axis_value120 = 0;
                state.axis_discrete = 0;
                if delta != 0 {
                    state.cycle_tasks(delta > 0);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for Panel {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer.ack_configure(serial);
                let width = width.max(1);
                let height = height.max(1);
                if width > MAX_PANEL_DIMENSION
                    || height > MAX_PANEL_DIMENSION
                    || u64::from(width).saturating_mul(u64::from(height)) > MAX_PANEL_PIXELS
                {
                    state.fatal_error = Some(format!(
                        "compositor configured an oversized panel buffer ({width}x{height})"
                    ));
                    state.closed = true;
                    return;
                }
                state.width = width;
                state.height = height;
                state.dirty = true;
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()> for Panel {
    fn event(
        _state: &mut Self,
        _list: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        _event: ext_foreign_toplevel_list_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(Panel, ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ())
    ]);
}

impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()> for Panel {
    fn event(
        _state: &mut Self,
        handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(event, ext_foreign_toplevel_handle_v1::Event::Closed) {
            handle.destroy();
        }
    }
}

impl Dispatch<ext_workspace_manager_v1::ExtWorkspaceManagerV1, ()> for Panel {
    fn event(
        _state: &mut Self,
        _manager: &ext_workspace_manager_v1::ExtWorkspaceManagerV1,
        _event: ext_workspace_manager_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(Panel, ext_workspace_manager_v1::ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ext_workspace_handle_v1::ExtWorkspaceHandleV1, ())
    ]);
}

delegate_noop!(Panel: ignore ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1);

impl Dispatch<ext_workspace_handle_v1::ExtWorkspaceHandleV1, ()> for Panel {
    fn event(
        state: &mut Self,
        handle: &ext_workspace_handle_v1::ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        let Some(index) = state
            .workspaces
            .iter()
            .position(|workspace| workspace.handle == *handle)
            .or_else(|| {
                if state.workspaces.len() >= MAX_WORKSPACES {
                    handle.destroy();
                    return None;
                }
                state.workspaces.push(Workspace {
                    handle: handle.clone(),
                    name: state.workspaces.len().saturating_add(1).to_string(),
                    active: false,
                });
                Some(state.workspaces.len() - 1)
            })
        else {
            return;
        };
        match event {
            ext_workspace_handle_v1::Event::Name { name } => state.workspaces[index].name = name,
            ext_workspace_handle_v1::Event::State {
                state: WEnum::Value(value),
            } => {
                state.workspaces[index].active =
                    value.contains(ext_workspace_handle_v1::State::Active);
            }
            ext_workspace_handle_v1::Event::Removed => {
                state.workspaces.remove(index);
            }
            _ => {}
        }
        state.dirty = true;
    }
}

impl Dispatch<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, ()> for Panel {
    fn event(
        _state: &mut Self,
        _manager: &zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
        _event: zwlr_foreign_toplevel_manager_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(Panel, zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ())
    ]);
}

impl Dispatch<zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ()> for Panel {
    fn event(
        state: &mut Self,
        handle: &zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        let Some(index) = state
            .tasks
            .iter()
            .position(|task| task.handle == *handle)
            .or_else(|| {
                if state.tasks.len() >= MAX_TASKS {
                    handle.destroy();
                    return None;
                }
                state.tasks.push(Task {
                    handle: handle.clone(),
                    title: String::new(),
                    app_id: String::new(),
                    active: false,
                    iconified: false,
                    outputs: Vec::new(),
                });
                Some(state.tasks.len() - 1)
            })
        else {
            return;
        };
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                state.tasks[index].title = title;
            }
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                state.tasks[index].app_id = app_id;
            }
            zwlr_foreign_toplevel_handle_v1::Event::State { state: bytes } => {
                let values = bytes
                    .chunks_exact(4)
                    .map(|bytes| u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                    .collect::<Vec<_>>();
                state.tasks[index].active =
                    values.contains(&(zwlr_foreign_toplevel_handle_v1::State::Activated as u32));
                state.tasks[index].iconified =
                    values.contains(&(zwlr_foreign_toplevel_handle_v1::State::Minimized as u32));
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputEnter { output }
                if !state.tasks[index].outputs.contains(&output) =>
            {
                state.tasks[index].outputs.push(output);
            }
            zwlr_foreign_toplevel_handle_v1::Event::OutputLeave { output } => {
                state.tasks[index]
                    .outputs
                    .retain(|candidate| *candidate != output);
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                handle.destroy();
                state.tasks.remove(index);
            }
            _ => {}
        }
        state.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::{argb, blend};
    use nobox_config::RgbColor;

    #[test]
    fn opaque_and_half_coverage_blending_are_bounded() {
        let black = argb(RgbColor::new(0, 0, 0));
        let white = argb(RgbColor::new(255, 255, 255));
        assert_eq!(blend(black, white, 0), black);
        assert_eq!(blend(black, white, 255), white);
        assert_eq!(blend(black, white, 128), 0xff80_8080);
    }
}
