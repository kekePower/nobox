//! `gnome-screenshot`-compatible X11 screenshot command with JPEG quality control.

use anyhow::{Context, Result, bail};
use chrono::Local;
use clap::{ArgAction, Parser, ValueEnum};
use nobox_screenshot::{
    Capture, CursorImage, DEFAULT_JPEG_QUALITY, ImageFormat, blend_cursor, encode, x11_to_rgb,
};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use x11rb::CURRENT_TIME;
use x11rb::NONE;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::ConnectionExt as XfixesConnectionExt;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt, CreateGCAux, CreateWindowAux, EventMask, GrabMode, GrabStatus,
    ImageFormat as XImageFormat, PropMode, Rectangle, SelectionNotifyEvent, SelectionRequestEvent,
    SubwindowMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;

const MAX_CAPTURE_PIXELS: u64 = 67_108_864;

#[derive(Debug, Parser)]
#[command(
    name = "nobox-screenshot",
    version,
    about = "Capture X11 screenshots with gnome-screenshot-compatible options",
    after_help = "JPEG example for agent use: nobox-screenshot --format jpeg --quality 75 --file shot.jpg"
)]
struct Cli {
    /// Send the grab directly to the clipboard.
    #[arg(short, long)]
    clipboard: bool,

    /// Grab the active window instead of the entire screen.
    #[arg(short, long, conflicts_with_all = ["area", "interactive"])]
    window: bool,

    /// Interactively drag out an area of the screen.
    #[arg(short, long, conflicts_with_all = ["window", "interactive"])]
    area: bool,

    /// Include the window border (accepted for compatibility; borders are always included).
    #[arg(short = 'b', long, action = ArgAction::SetTrue)]
    include_border: bool,

    /// Remove the window border (deprecated compatibility option; ignored).
    #[arg(short = 'B', long, action = ArgAction::SetTrue, conflicts_with = "include_border")]
    remove_border: bool,

    /// Include the pointer in the screenshot.
    #[arg(short = 'p', long)]
    include_pointer: bool,

    /// Take the screenshot after this many seconds.
    #[arg(short = 'd', long, value_name = "seconds", default_value_t = 0)]
    delay: u64,

    /// Deprecated border effect (accepted and ignored like gnome-screenshot 41).
    #[arg(short = 'e', long, value_name = "effect", value_enum, default_value_t = BorderEffect::None)]
    border_effect: BorderEffect,

    /// Start an interactive area selection.
    #[arg(short = 'i', long, conflicts_with_all = ["window", "area"])]
    interactive: bool,

    /// Save directly to this file.
    #[arg(short = 'f', long, value_name = "filename")]
    file: Option<PathBuf>,

    /// JPEG quality from 1 (smallest) to 100 (best); implies JPEG unless format is explicit.
    #[arg(short = 'q', long, value_name = "1-100", value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: Option<u8>,

    /// Output encoding. The filename extension is used when omitted.
    #[arg(long, value_enum)]
    format: Option<ImageFormat>,

    /// Write encoded image bytes to stdout.
    #[arg(long, conflicts_with = "clipboard")]
    stdout: bool,

    /// X display to use.
    #[arg(long, value_name = "DISPLAY")]
    display: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum BorderEffect {
    Shadow,
    Border,
    Vintage,
    #[default]
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Region {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("nobox-screenshot: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let format = resolve_format(&cli)?;
    if format == ImageFormat::Png && cli.quality.is_some() {
        bail!("--quality controls lossy JPEG encoding and cannot be combined with --format png");
    }
    if cli.delay != 0 {
        thread::sleep(Duration::from_secs(cli.delay));
    }

    let (connection, screen_index) = RustConnection::connect(cli.display.as_deref())
        .with_context(|| display_context(cli.display.as_deref()))?;
    let screen = &connection.setup().roots[screen_index];
    let root = screen.root;
    let region = if cli.area || cli.interactive {
        select_region(&connection, root)?
    } else if cli.window {
        active_window_region(
            &connection,
            root,
            screen.width_in_pixels,
            screen.height_in_pixels,
        )?
    } else {
        Region {
            x: 0,
            y: 0,
            width: screen.width_in_pixels,
            height: screen.height_in_pixels,
        }
    };
    let mut capture = capture_root(&connection, screen_index, root, region)?;
    if cli.include_pointer {
        include_pointer(&connection, &mut capture)?;
    }
    let quality = cli.quality.unwrap_or(DEFAULT_JPEG_QUALITY);
    let mut encoded = Vec::new();
    encode(&mut encoded, &capture, format, quality)?;

    if cli.stdout {
        io::stdout()
            .lock()
            .write_all(&encoded)
            .context("could not write the screenshot to stdout")?;
    } else if cli.clipboard {
        persist_clipboard(&connection, screen_index, format, &encoded)?;
    } else {
        let path = write_output(cli.file.as_deref(), format, &encoded)?;
        println!("{}", path.display());
    }
    Ok(())
}

fn display_context(display: Option<&str>) -> String {
    display.map_or_else(
        || {
            "could not connect to the X display; native Wayland capture is not supported yet"
                .to_owned()
        },
        |display| format!("could not connect to X display {display:?}"),
    )
}

fn resolve_format(cli: &Cli) -> Result<ImageFormat> {
    if let Some(format) = cli.format {
        return Ok(format);
    }
    if let Some(path) = cli.file.as_deref() {
        if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
            return match extension.to_ascii_lowercase().as_str() {
                "png" => Ok(ImageFormat::Png),
                "jpg" | "jpeg" => Ok(ImageFormat::Jpeg),
                other => {
                    bail!("cannot infer an image format from .{other}; use --format png or jpeg")
                }
            };
        }
    }
    Ok(if cli.quality.is_some() {
        ImageFormat::Jpeg
    } else {
        ImageFormat::Png
    })
}

fn capture_root(
    connection: &RustConnection,
    screen_index: usize,
    root: Window,
    region: Region,
) -> Result<Capture> {
    let pixels = u64::from(region.width) * u64::from(region.height);
    if pixels == 0 || pixels > MAX_CAPTURE_PIXELS {
        bail!("capture must contain between 1 and {MAX_CAPTURE_PIXELS} pixels");
    }
    let reply = connection
        .get_image(
            XImageFormat::Z_PIXMAP,
            root,
            region.x,
            region.y,
            region.width,
            region.height,
            !0,
        )?
        .reply()
        .context("the X server refused the screenshot")?;
    let rgb = x11_to_rgb(
        connection.setup(),
        screen_index,
        region.width,
        region.height,
        reply.depth,
        &reply.data,
    )?;
    Ok(Capture {
        width: region.width,
        height: region.height,
        x: region.x,
        y: region.y,
        rgb,
    })
}

fn include_pointer(connection: &RustConnection, capture: &mut Capture) -> Result<()> {
    let version = connection
        .xfixes_query_version(4, 0)?
        .reply()
        .context("XFixes is unavailable, so the pointer cannot be included")?;
    if version.major_version < 2 {
        bail!("XFixes 2.0 is required to include the pointer");
    }
    let cursor = connection
        .xfixes_get_cursor_image()?
        .reply()
        .context("could not read the current X11 cursor")?;
    blend_cursor(
        capture,
        CursorImage {
            x: cursor.x,
            y: cursor.y,
            hotspot_x: cursor.xhot,
            hotspot_y: cursor.yhot,
            width: cursor.width,
            height: cursor.height,
            argb: &cursor.cursor_image,
        },
    );
    Ok(())
}

fn active_window_region(
    connection: &RustConnection,
    root: Window,
    screen_width: u16,
    screen_height: u16,
) -> Result<Region> {
    let active_atom = intern_atom(connection, b"_NET_ACTIVE_WINDOW")?;
    let reply = connection
        .get_property(false, root, active_atom, AtomEnum::WINDOW, 0, 1)?
        .reply()?;
    let mut window = reply
        .value32()
        .and_then(|mut values| values.next())
        .filter(|window| *window != NONE)
        .context("the window manager did not publish an active window")?;
    loop {
        let tree = connection.query_tree(window)?.reply()?;
        if tree.parent == root || tree.parent == NONE {
            break;
        }
        window = tree.parent;
    }
    let geometry = connection.get_geometry(window)?.reply()?;
    let border = i16::try_from(geometry.border_width).context("window border is too wide")?;
    let translated = connection
        .translate_coordinates(window, root, -border, -border)?
        .reply()?;
    clip_region(
        i32::from(translated.dst_x),
        i32::from(translated.dst_y),
        u32::from(geometry.width) + 2 * u32::from(geometry.border_width),
        u32::from(geometry.height) + 2 * u32::from(geometry.border_width),
        screen_width,
        screen_height,
    )
}

fn clip_region(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    screen_width: u16,
    screen_height: u16,
) -> Result<Region> {
    let left = x.max(0).min(i32::from(screen_width));
    let top = y.max(0).min(i32::from(screen_height));
    let right = x
        .saturating_add(i32::try_from(width).unwrap_or(i32::MAX))
        .max(0)
        .min(i32::from(screen_width));
    let bottom = y
        .saturating_add(i32::try_from(height).unwrap_or(i32::MAX))
        .max(0)
        .min(i32::from(screen_height));
    if right <= left || bottom <= top {
        bail!("the active window is entirely outside the visible screen");
    }
    Ok(Region {
        x: i16::try_from(left).context("capture X coordinate is outside X11 range")?,
        y: i16::try_from(top).context("capture Y coordinate is outside X11 range")?,
        width: u16::try_from(right - left).context("capture width is outside X11 range")?,
        height: u16::try_from(bottom - top).context("capture height is outside X11 range")?,
    })
}

fn select_region(connection: &RustConnection, root: Window) -> Result<Region> {
    let reply = connection
        .grab_pointer(
            false,
            root,
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            NONE,
            NONE,
            CURRENT_TIME,
        )?
        .reply()?;
    if reply.status != GrabStatus::SUCCESS {
        bail!(
            "could not grab the pointer for area selection: {:?}",
            reply.status
        );
    }
    let keyboard = connection
        .grab_keyboard(false, root, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)?
        .reply()?;
    if keyboard.status != GrabStatus::SUCCESS {
        connection.ungrab_pointer(CURRENT_TIME)?;
        bail!("could not grab the keyboard for area selection");
    }

    let gc = connection.generate_id()?;
    connection.create_gc(
        gc,
        root,
        &CreateGCAux::new()
            .function(x11rb::protocol::xproto::GX::XOR)
            .foreground(!0)
            .subwindow_mode(SubwindowMode::INCLUDE_INFERIORS)
            .line_width(1),
    )?;
    connection.flush()?;

    let result = selection_loop(connection, root, gc);
    let _ = connection.ungrab_pointer(CURRENT_TIME);
    let _ = connection.ungrab_keyboard(CURRENT_TIME);
    let _ = connection.free_gc(gc);
    let _ = connection.flush();
    result
}

fn selection_loop(connection: &RustConnection, root: Window, gc: u32) -> Result<Region> {
    let mut start = None;
    let mut last = None;
    loop {
        match connection.wait_for_event()? {
            Event::ButtonPress(event) if event.detail == 1 => {
                start = Some((event.root_x, event.root_y));
            }
            Event::MotionNotify(event) => {
                let Some(origin) = start else { continue };
                if let Some(previous) = last {
                    draw_selection(connection, root, gc, origin, previous)?;
                }
                let current = (event.root_x, event.root_y);
                draw_selection(connection, root, gc, origin, current)?;
                connection.flush()?;
                last = Some(current);
            }
            Event::ButtonRelease(event) if event.detail == 1 => {
                let Some(origin) = start else { continue };
                if let Some(previous) = last {
                    draw_selection(connection, root, gc, origin, previous)?;
                    connection.flush()?;
                }
                return region_between(origin, (event.root_x, event.root_y));
            }
            Event::KeyPress(event) => {
                let mapping = connection.get_keyboard_mapping(event.detail, 1)?.reply()?;
                if mapping.keysyms.contains(&0xff1b) {
                    bail!("area selection was cancelled");
                }
            }
            _ => {}
        }
    }
}

fn draw_selection(
    connection: &RustConnection,
    root: Window,
    gc: u32,
    first: (i16, i16),
    second: (i16, i16),
) -> Result<()> {
    if let Ok(region) = region_between(first, second) {
        connection.poly_rectangle(
            root,
            gc,
            &[Rectangle {
                x: region.x,
                y: region.y,
                width: region.width.saturating_sub(1),
                height: region.height.saturating_sub(1),
            }],
        )?;
    }
    Ok(())
}

fn region_between(first: (i16, i16), second: (i16, i16)) -> Result<Region> {
    let left = first.0.min(second.0);
    let top = first.1.min(second.1);
    let width = u16::try_from(i32::from(first.0).abs_diff(i32::from(second.0))).unwrap_or(u16::MAX);
    let height =
        u16::try_from(i32::from(first.1).abs_diff(i32::from(second.1))).unwrap_or(u16::MAX);
    if width == 0 || height == 0 {
        bail!("the selected area is empty");
    }
    Ok(Region {
        x: left,
        y: top,
        width,
        height,
    })
}

fn write_output(requested: Option<&Path>, format: ImageFormat, data: &[u8]) -> Result<PathBuf> {
    if let Some(path) = requested {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("could not create {}", path.display()))?;
        finish_file(&mut file, path, data)?;
        return Ok(path.to_owned());
    }
    let directory = std::env::var_os("XDG_PICTURES_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join("Pictures"))
                .filter(|path| path.is_dir())
        })
        .unwrap_or(std::env::current_dir().context("could not resolve an output directory")?);
    let stem = format!(
        "Screenshot from {}",
        Local::now().format("%Y-%m-%d %H-%M-%S")
    );
    for suffix in 0..=100 {
        let filename = if suffix == 0 {
            format!("{stem}.{}", format.extension())
        } else {
            format!("{stem} ({suffix}).{}", format.extension())
        };
        let path = directory.join(filename);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                finish_file(&mut file, &path, data)?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("could not create {}", path.display()));
            }
        }
    }
    bail!(
        "could not choose a unique screenshot filename in {}",
        directory.display()
    )
}

fn finish_file(file: &mut File, path: &Path, data: &[u8]) -> Result<()> {
    file.write_all(data)
        .with_context(|| format!("could not write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("could not finish {}", path.display()))
}

fn persist_clipboard(
    connection: &RustConnection,
    screen_index: usize,
    format: ImageFormat,
    data: &[u8],
) -> Result<()> {
    let clipboard = intern_atom(connection, b"CLIPBOARD")?;
    let clipboard_manager = intern_atom(connection, b"CLIPBOARD_MANAGER")?;
    let save_targets = intern_atom(connection, b"SAVE_TARGETS")?;
    let targets = intern_atom(connection, b"TARGETS")?;
    let mime = intern_atom(connection, format.mime_type().as_bytes())?;
    let property = intern_atom(connection, b"NOBOX_SCREENSHOT_CLIPBOARD")?;
    let manager = connection
        .get_selection_owner(clipboard_manager)?
        .reply()?
        .owner;
    if manager == NONE {
        bail!("no X11 clipboard manager is running; use --file or --stdout instead");
    }

    let screen = &connection.setup().roots[screen_index];
    let owner = connection.generate_id()?;
    connection.create_window(
        x11rb::COPY_FROM_PARENT as u8,
        owner,
        screen.root,
        -1,
        -1,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?;
    connection.set_selection_owner(owner, clipboard, CURRENT_TIME)?;
    if connection.get_selection_owner(clipboard)?.reply()?.owner != owner {
        bail!("could not claim the X11 clipboard");
    }
    connection.convert_selection(
        owner,
        clipboard_manager,
        save_targets,
        property,
        CURRENT_TIME,
    )?;
    connection.flush()?;

    loop {
        match connection.wait_for_event()? {
            Event::SelectionRequest(event) if event.owner == owner => {
                serve_selection(connection, event, targets, mime, data)?;
            }
            Event::SelectionNotify(event)
                if event.requestor == owner
                    && event.selection == clipboard_manager
                    && event.target == save_targets =>
            {
                if event.property == NONE {
                    bail!("the clipboard manager could not persist the screenshot");
                }
                connection.destroy_window(owner)?;
                connection.flush()?;
                return Ok(());
            }
            Event::SelectionClear(event) if event.selection == clipboard => {
                bail!("clipboard ownership was lost before the screenshot was persisted");
            }
            _ => {}
        }
    }
}

fn serve_selection(
    connection: &RustConnection,
    event: SelectionRequestEvent,
    targets: Atom,
    mime: Atom,
    data: &[u8],
) -> Result<()> {
    let property = if event.property == NONE {
        event.target
    } else {
        event.property
    };
    let accepted = if event.target == targets {
        connection.change_property32(
            PropMode::REPLACE,
            event.requestor,
            property,
            AtomEnum::ATOM,
            &[targets, mime],
        )?;
        true
    } else if event.target == mime {
        let maximum = connection.maximum_request_bytes().saturating_sub(24);
        if data.len() > maximum {
            false
        } else {
            connection.change_property8(
                PropMode::REPLACE,
                event.requestor,
                property,
                mime,
                data,
            )?;
            true
        }
    } else {
        false
    };
    let response = SelectionNotifyEvent {
        response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: event.time,
        requestor: event.requestor,
        selection: event.selection,
        target: event.target,
        property: if accepted { property } else { NONE },
    };
    connection.send_event(false, event.requestor, EventMask::NO_EVENT, response)?;
    connection.flush()?;
    Ok(())
}

fn intern_atom(connection: &RustConnection, name: &[u8]) -> Result<Atom> {
    connection
        .intern_atom(false, name)?
        .reply()
        .map(|reply| reply.atom)
        .with_context(|| {
            format!(
                "could not intern X11 atom {}",
                String::from_utf8_lossy(name)
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_geometry_is_normalized() {
        assert_eq!(
            region_between((90, 70), (10, 20)).unwrap(),
            Region {
                x: 10,
                y: 20,
                width: 80,
                height: 50,
            }
        );
        assert!(region_between((10, 20), (10, 30)).is_err());
    }

    #[test]
    fn screen_clipping_handles_partly_offscreen_windows() {
        assert_eq!(
            clip_region(-20, -10, 100, 50, 640, 480).unwrap(),
            Region {
                x: 0,
                y: 0,
                width: 80,
                height: 40,
            }
        );
        assert!(clip_region(700, 0, 10, 10, 640, 480).is_err());
    }
}
