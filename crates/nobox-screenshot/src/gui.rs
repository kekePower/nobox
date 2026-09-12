//! Optional native capture chooser; capture itself stays in the CLI path.

use super::Cli;
use anyhow::{Context, Result, bail};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::Cell;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::rc::Rc;

#[derive(Clone, Copy)]
struct Options {
    window: bool,
    area: bool,
    pointer: bool,
    delay: u64,
    sound: bool,
    flash: bool,
}

pub(super) fn choose_options(cli: &mut Cli) -> Result<bool> {
    if cli.delay > 3600 {
        bail!("the options window supports delays from 0 to 3600 seconds");
    }
    // Set the child's environment before GTK can start threads. Re-exec only
    // when needed, so --display also works with an unset/invalid DISPLAY and
    // a Wayland desktop cannot put the chooser on a different backend.
    let display = cli.display.as_deref().map(std::ffi::OsStr::new);
    if std::env::var("GDK_BACKEND").as_deref() != Ok("x11")
        || display.is_some_and(|name| std::env::var_os("DISPLAY").as_deref() != Some(name))
    {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(std::env::args_os().skip(1))
            .env("GDK_BACKEND", "x11");
        if let Some(display) = display {
            command.env("DISPLAY", display);
        }
        return Err(command.exec()).context("could not start the screenshot options window");
    }
    gdk::set_allowed_backends("x11");
    gtk::init().context("could not open the screenshot options window on the X display")?;

    let window = gtk::Window::builder()
        .title("Screenshot")
        .default_width(420)
        .resizable(false)
        .build();
    let header = gtk::HeaderBar::new();
    header.set_show_title_buttons(false);
    let cancel = gtk::Button::with_mnemonic("_Cancel");
    let capture = gtk::Button::with_mnemonic("_Take screenshot");
    capture.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&capture);
    window.set_titlebar(Some(&header));
    window.set_default_widget(Some(&capture));

    let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_top(24);
    body.set_margin_bottom(24);
    let heading = gtk::Label::new(Some("Capture area"));
    heading.set_xalign(0.0);
    heading.add_css_class("heading");
    body.append(&heading);
    let modes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    modes.add_css_class("linked");
    modes.set_homogeneous(true);
    let screen = mode_button("_Screen", "video-display-symbolic");
    let active_window = mode_button("_Window", "window-maximize-symbolic");
    let area = mode_button("Se_lection", "edit-select-all-symbolic");
    active_window.set_group(Some(&screen));
    area.set_group(Some(&screen));
    for button in [&screen, &active_window, &area] {
        modes.append(button);
    }
    body.append(&modes);

    let hint = gtk::Label::new(None);
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    hint.set_max_width_chars(44);
    hint.add_css_class("dim-label");
    for (button, text) in [
        (&screen, "Captures the entire screen."),
        (
            &active_window,
            "Captures the window active when the delay ends.",
        ),
        (&area, "Drag to select an area after this window closes."),
    ] {
        let hint = hint.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                hint.set_text(text);
            }
        });
    }
    if cli.window {
        active_window.set_active(true);
    } else if cli.area {
        area.set_active(true);
    } else {
        screen.set_active(true);
    }
    body.append(&hint);
    let pointer = switch_row(&body, "Show _pointer", cli.include_pointer);
    let delay = gtk::SpinButton::with_range(0.0, 3600.0, 1.0);
    delay.set_numeric(true);
    delay.set_value(cli.delay as f64);
    append_row(&body, "_Delay (seconds)", &delay);
    body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let sound = switch_row(&body, "Shutter sou_nd", !cli.no_sound && !cli.stdout);
    let flash = switch_row(
        &body,
        "Window capture _outline",
        !cli.no_flash && !cli.stdout,
    );
    sound.set_sensitive(!cli.stdout);
    flash.set_sensitive(!cli.stdout && cli.window);
    let flash_control = flash.clone();
    let stdout = cli.stdout;
    active_window.connect_toggled(move |button| {
        flash_control.set_sensitive(!stdout && button.is_active());
    });
    window.set_child(Some(&body));

    let result = Rc::new(Cell::new(None));
    let selected = result.clone();
    let weak_window = window.downgrade();
    capture.connect_clicked(move |_| {
        selected.set(Some(Options {
            window: active_window.is_active(),
            area: area.is_active(),
            pointer: pointer.is_active(),
            delay: delay.value_as_int() as u64,
            sound: sound.is_active(),
            flash: flash.is_active(),
        }));
        if let Some(window) = weak_window.upgrade() {
            window.close();
        }
    });
    let weak_window = window.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(window) = weak_window.upgrade() {
            window.close();
        }
    });
    run_window(&window);
    let Some(options) = result.get() else {
        return Ok(false);
    };
    cli.window = options.window;
    cli.area = options.area;
    cli.include_pointer = options.pointer;
    cli.delay = options.delay;
    cli.no_sound = !options.sound;
    cli.no_flash = !options.flash;
    Ok(true)
}

fn mode_button(label: &str, icon: &str) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::new();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(32);
    content.append(&image);
    let label = gtk::Label::with_mnemonic(label);
    label.set_mnemonic_widget(Some(&button));
    content.append(&label);
    button.set_child(Some(&content));
    button
}

fn append_row(body: &gtk::Box, text: &str, control: &impl IsA<gtk::Widget>) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    let label = gtk::Label::with_mnemonic(text);
    label.set_mnemonic_widget(Some(control));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    control.set_valign(gtk::Align::Center);
    row.append(&label);
    row.append(control);
    body.append(&row);
}

fn switch_row(body: &gtk::Box, text: &str, active: bool) -> gtk::Switch {
    let switch = gtk::Switch::builder().active(active).build();
    append_row(body, text, &switch);
    switch
}

fn run_window(window: &gtk::Window) {
    let main_loop = glib::MainLoop::new(None, false);
    let close_loop = main_loop.clone();
    window.connect_close_request(move |_| {
        close_loop.quit();
        glib::Propagation::Proceed
    });
    let keys = gtk::EventControllerKey::new();
    let weak_window = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            if let Some(window) = weak_window.upgrade() {
                window.close();
            }
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(keys);
    window.present();
    main_loop.run();
    window.destroy();
    if let Some(display) = gdk::Display::default() {
        display.sync();
    }
}

pub(super) fn report_error(message: &str) {
    let window = gtk::Window::builder()
        .title("Screenshot failed")
        .default_width(420)
        .resizable(false)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_top(24);
    body.set_margin_bottom(24);
    let label = gtk::Label::new(Some(message));
    label.set_wrap(true);
    label.set_max_width_chars(50);
    label.set_selectable(true);
    body.append(&label);
    let close = gtk::Button::with_mnemonic("_Close");
    let weak_window = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = weak_window.upgrade() {
            window.close();
        }
    });
    body.append(&close);
    window.set_child(Some(&body));
    window.set_default_widget(Some(&close));
    run_window(&window);
}
