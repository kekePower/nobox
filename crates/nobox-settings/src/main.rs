//! Native GTK/libadwaita settings application for nobox.

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    process::ExitCode,
    rc::Rc,
    time::Duration,
};

use adw::prelude::*;
use clap::Parser;
use gtk::{gdk, gio, glib};
use nobox_config::{Config, RgbColor, TitleAlignment, config_path};
use nobox_settings::{SettingKey, SettingValue, SettingsDocument};

#[derive(Debug, Parser)]
#[command(version, about = "Configure nobox through its validated TOML model")]
struct Cli {
    /// Edit a specific configuration file.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Exercise a mapped settings window and one save for the integration test.
    #[arg(long, hide = true)]
    test_save_follow_mouse: bool,
}

struct UiState {
    path: PathBuf,
    document: RefCell<SettingsDocument>,
    saved_source: RefCell<String>,
    source: gtk::TextBuffer,
    status: gtk::Label,
    preview: gtk::DrawingArea,
    synchronizing: Cell<bool>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let path = match cli.config.map_or_else(config_path, Ok) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("nobox-settings: {error}");
            return ExitCode::FAILURE;
        }
    };
    let document = match SettingsDocument::load(&path) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("nobox-settings: {error}");
            return ExitCode::FAILURE;
        }
    };
    let failed = Rc::new(Cell::new(false));
    let app = gtk::Application::builder()
        .application_id("com.kekepower.nobox.Settings")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_startup(|_| {
        if let Err(error) = adw::init() {
            eprintln!("nobox-settings: could not initialize libadwaita: {error}");
        }
    });
    let failed_activate = Rc::clone(&failed);
    app.connect_activate(move |app| {
        let state = match build_window(app, path.clone(), document.clone()) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("nobox-settings: {error}");
                failed_activate.set(true);
                app.quit();
                return;
            }
        };
        if cli.test_save_follow_mouse {
            let app = app.clone();
            let failed = Rc::clone(&failed_activate);
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                apply_setting(&state, SettingKey::FollowMouse, SettingValue::Boolean(true));
                if let Err(error) = save(&state) {
                    eprintln!("nobox-settings: integration save failed: {error}");
                    failed.set(true);
                } else {
                    println!("settings window mapped and saved {}", state.path.display());
                }
                app.quit();
            });
        }
    });
    let arguments = ["nobox-settings"];
    let result = app.run_with_args(&arguments);
    if failed.get() || result != glib::ExitCode::SUCCESS {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn build_window(
    app: &gtk::Application,
    path: PathBuf,
    document: SettingsDocument,
) -> Result<Rc<UiState>, nobox_settings::SettingsError> {
    let config = document.config()?;
    let original_source = document.source();
    let source = gtk::TextBuffer::new(None);
    source.set_text(&original_source);
    let status = gtk::Label::builder()
        .label(format!("Editing {}", path.display()))
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["dim-label"])
        .build();
    let preview = gtk::DrawingArea::builder()
        .height_request(132)
        .hexpand(true)
        .build();
    let state = Rc::new(UiState {
        path,
        document: RefCell::new(document),
        saved_source: RefCell::new(original_source),
        source,
        status,
        preview,
        synchronizing: Cell::new(false),
    });

    let stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_titled(
        &scroll_page(build_behavior_page(&state, &config)),
        Some("behavior"),
        "Behavior",
    );
    stack.add_titled(
        &scroll_page(build_workspace_page(&state, &config)),
        Some("workspaces"),
        "Workspaces",
    );
    stack.add_titled(
        &scroll_page(build_appearance_page(&state, &config)),
        Some("appearance"),
        "Appearance",
    );
    stack.add_titled(
        &build_advanced_page(&state),
        Some("advanced"),
        "Advanced TOML",
    );

    let sidebar = gtk::StackSidebar::builder()
        .stack(&stack)
        .width_request(190)
        .vexpand(true)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&sidebar);
    content.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    content.append(&stack);

    let save_button = gtk::Button::builder()
        .label("Save changes")
        .css_classes(["suggested-action"])
        .build();
    let state_save = Rc::clone(&state);
    save_button.connect_clicked(move |_| {
        if let Err(error) = save(&state_save) {
            show_error(&state_save, &error.to_string());
        }
    });
    let title = adw::WindowTitle::new("nobox preferences", "Policy first. Backend second.");
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    header.pack_end(&save_button);

    let status_bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(8)
        .margin_end(12)
        .margin_bottom(8)
        .margin_start(12)
        .build();
    status_bar.append(&state.status);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&content);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&status_bar);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("nobox preferences")
        .default_width(980)
        .default_height(720)
        .content(&root)
        .build();
    let state_close = Rc::clone(&state);
    window.connect_close_request(move |window| {
        if buffer_text(&state_close.source) == *state_close.saved_source.borrow() {
            return glib::Propagation::Proceed;
        }
        let dialog = adw::MessageDialog::new(
            Some(window),
            Some("Discard unsaved changes?"),
            Some("Your saved nobox configuration will remain unchanged."),
        );
        dialog.add_response("cancel", "Keep editing");
        dialog.add_response("discard", "Discard changes");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        let window = window.clone();
        dialog.connect_response(Some("discard"), move |_, _| {
            window.destroy();
        });
        dialog.present();
        glib::Propagation::Stop
    });
    install_preview(&state);
    let state_source = Rc::clone(&state);
    state.source.connect_changed(move |_| {
        if !state_source.synchronizing.get() {
            show_status(&state_source, "Unsaved advanced edits", false);
        }
    });
    window.present();
    Ok(state)
}

fn page_box() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(24)
        .margin_end(32)
        .margin_bottom(32)
        .margin_start(32)
        .build()
}

fn scroll_page(content: gtk::Box) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(760)
        .tightening_threshold(560)
        .child(&content)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build()
}

fn build_behavior_page(state: &Rc<UiState>, config: &Config) -> gtk::Box {
    let page = page_box();
    let focus = adw::PreferencesGroup::builder()
        .title("Focus contract")
        .description("Choose when nobox may move keyboard focus. These rules apply consistently across workspaces.")
        .build();
    add_switch(
        &focus,
        state,
        SettingKey::FocusNew,
        "Focus new windows",
        "Give eligible new windows focus when they appear.",
        config.focus.focus_new,
    );
    add_switch(
        &focus,
        state,
        SettingKey::FollowMouse,
        "Follow the pointer",
        "Focus a window when the pointer enters it.",
        config.focus.follow_mouse,
    );
    add_switch(
        &focus,
        state,
        SettingKey::PreventFocusStealing,
        "Prevent focus stealing",
        "Mark stale requests urgent instead of interrupting work.",
        config.focus.prevent_focus_stealing,
    );
    add_switch(
        &focus,
        state,
        SettingKey::RaiseOnFocus,
        "Raise focused windows",
        "Keep the focused window above overlapping peers.",
        config.focus.raise_on_focus,
    );
    page.append(&focus);

    let interaction = adw::PreferencesGroup::builder()
        .title("Interaction")
        .description("Small physical thresholds that make pointer operations feel predictable.")
        .build();
    add_spin(
        &interaction,
        state,
        SettingKey::EdgeResistance,
        "Edge resistance",
        "Pixels from a work-area edge before snapping.",
        config.mouse.edge_resistance,
        0,
        256,
        1,
    );
    add_spin(
        &interaction,
        state,
        SettingKey::DragThreshold,
        "Drag threshold",
        "Pointer travel required before a drag begins.",
        config.mouse.drag_threshold,
        0,
        256,
        1,
    );
    add_spin(
        &interaction,
        state,
        SettingKey::DoubleClickMs,
        "Double-click interval",
        "Maximum delay between titlebar clicks, in milliseconds.",
        config.mouse.double_click_ms,
        100,
        2_000,
        25,
    );
    add_switch(
        &interaction,
        state,
        SettingKey::CenterFreeSpace,
        "Center free placement",
        "Center a new window inside the first completely free field.",
        config.placement.center_free_space,
    );
    page.append(&interaction);

    let overlays = adw::PreferencesGroup::builder()
        .title("Transient overlays")
        .description("Switcher and menu dimensions stay bounded by the active output.")
        .build();
    add_switch(
        &overlays,
        state,
        SettingKey::SwitcherEnabled,
        "Show the window switcher",
        "Display titles while cycling with a held modifier.",
        config.switcher.enabled,
    );
    add_spin(
        &overlays,
        state,
        SettingKey::SwitcherWidth,
        "Switcher width",
        "Preferred width in pixels.",
        config.switcher.width,
        160,
        1_024,
        10,
    );
    add_spin(
        &overlays,
        state,
        SettingKey::SwitcherRowHeight,
        "Switcher row height",
        "Height of each visible title row.",
        config.switcher.row_height,
        16,
        64,
        1,
    );
    add_spin(
        &overlays,
        state,
        SettingKey::SwitcherMaxRows,
        "Switcher rows",
        "Maximum visible rows before scrolling.",
        config.switcher.max_rows,
        1,
        32,
        1,
    );
    add_spin(
        &overlays,
        state,
        SettingKey::MenuWidth,
        "Menu width",
        "Preferred popup width in pixels.",
        config.menu.width,
        120,
        1_024,
        10,
    );
    add_spin(
        &overlays,
        state,
        SettingKey::MenuRowHeight,
        "Menu row height",
        "Height of menu titles and entries.",
        config.menu.row_height,
        16,
        64,
        1,
    );
    add_spin(
        &overlays,
        state,
        SettingKey::MenuMaxRows,
        "Menu rows",
        "Maximum visible rows before scrolling.",
        config.menu.max_rows,
        1,
        64,
        1,
    );
    page.append(&overlays);
    page
}

fn build_workspace_page(state: &Rc<UiState>, config: &Config) -> gtk::Box {
    let page = page_box();
    let group = adw::PreferencesGroup::builder()
        .title("Workspace map")
        .description("Names define the workspace count. Columns turn the ordered list into a directional grid.")
        .build();
    let names = config.workspaces.names.join(", ");
    add_text(
        &group,
        state,
        "Workspace names",
        "Comma-separated names, kept in this order.",
        &names,
        |text| {
            SettingValue::TextList(
                text.split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect(),
            )
        },
        SettingKey::WorkspaceNames,
    );
    add_spin(
        &group,
        state,
        SettingKey::WorkspaceColumns,
        "Grid columns",
        "Use zero for one row; otherwise choose a fixed column count.",
        config.workspaces.columns,
        0,
        32,
        1,
    );
    add_switch(
        &group,
        state,
        SettingKey::WorkspaceWrap,
        "Wrap at grid edges",
        "Continue from the opposite edge during directional navigation.",
        config.workspaces.wrap,
    );
    page.append(&group);

    let explanation = adw::PreferencesGroup::builder()
        .title("What stays stable")
        .description("Changing the count keeps existing windows on a valid workspace. Removing a workspace merges its clients into the final remaining workspace.")
        .build();
    page.append(&explanation);
    page
}

fn build_appearance_page(state: &Rc<UiState>, config: &Config) -> gtk::Box {
    let page = page_box();
    let preview_group = adw::PreferencesGroup::builder()
        .title("Window chrome specimen")
        .description("The active frame is shown at full height; inactive and urgent states remain visible as compact references.")
        .build();
    preview_group.add(&state.preview);
    page.append(&preview_group);

    let geometry = adw::PreferencesGroup::builder()
        .title("Frame geometry")
        .description("Nobox measures the selected X11 core font on the server before applying it.")
        .build();
    add_spin(
        &geometry,
        state,
        SettingKey::BorderWidth,
        "Border width",
        "Pixels around the framed client.",
        config.theme.border_width,
        0,
        64,
        1,
    );
    add_spin(
        &geometry,
        state,
        SettingKey::TitlebarHeight,
        "Titlebar height",
        "Set zero to disable the titlebar.",
        config.theme.titlebar_height,
        0,
        128,
        1,
    );
    add_spin(
        &geometry,
        state,
        SettingKey::TitlePadding,
        "Title padding",
        "Horizontal inset around the title text.",
        config.theme.title_padding,
        0,
        64,
        1,
    );
    add_text(
        &geometry,
        state,
        "X11 font",
        "Core font name or XLFD pattern.",
        &config.theme.font,
        |text| SettingValue::Text(text.to_owned()),
        SettingKey::Font,
    );
    add_alignment(&geometry, state, config.theme.title_alignment);
    page.append(&geometry);

    let palette = adw::PreferencesGroup::builder()
        .title("State palette")
        .description("A compact palette keeps focus and urgency legible without turning the frame into decoration for its own sake.")
        .build();
    for (key, title, color) in [
        (
            SettingKey::ActiveBorder,
            "Active border",
            config.theme.active_border,
        ),
        (
            SettingKey::InactiveBorder,
            "Inactive border",
            config.theme.inactive_border,
        ),
        (
            SettingKey::UrgentBorder,
            "Urgent border",
            config.theme.urgent_border,
        ),
        (
            SettingKey::ActiveTitlebar,
            "Active titlebar",
            config.theme.active_titlebar,
        ),
        (
            SettingKey::InactiveTitlebar,
            "Inactive titlebar",
            config.theme.inactive_titlebar,
        ),
        (
            SettingKey::UrgentTitlebar,
            "Urgent titlebar",
            config.theme.urgent_titlebar,
        ),
        (SettingKey::TitleText, "Title text", config.theme.title_text),
        (
            SettingKey::MinimizeButton,
            "Minimize button",
            config.theme.minimize_button,
        ),
        (
            SettingKey::MaximizeButton,
            "Maximize button",
            config.theme.maximize_button,
        ),
        (
            SettingKey::CloseButton,
            "Close button",
            config.theme.close_button,
        ),
        (
            SettingKey::ButtonGlyph,
            "Button glyph",
            config.theme.button_glyph,
        ),
    ] {
        add_color(&palette, state, key, title, color);
    }
    page.append(&palette);
    page
}

fn build_advanced_page(state: &Rc<UiState>) -> gtk::Box {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(20)
        .margin_end(24)
        .margin_bottom(24)
        .margin_start(24)
        .build();
    let heading = gtk::Label::builder()
        .label("Complete TOML")
        .xalign(0.0)
        .css_classes(["title-2"])
        .build();
    let description = gtk::Label::builder()
        .label("Bindings, menus, application rules, and every friendly setting live in this same document. Save validates the entire model before replacing the file.")
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    let text = gtk::TextView::builder()
        .buffer(&state.source)
        .monospace(true)
        .top_margin(16)
        .right_margin(16)
        .bottom_margin(16)
        .left_margin(16)
        .wrap_mode(gtk::WrapMode::None)
        .build();
    let scrolled = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .has_frame(true)
        .child(&text)
        .build();
    page.append(&heading);
    page.append(&description);
    page.append(&scrolled);
    page
}

fn add_switch(
    group: &adw::PreferencesGroup,
    state: &Rc<UiState>,
    key: SettingKey,
    title: &str,
    subtitle: &str,
    active: bool,
) {
    let row = adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .active(active)
        .build();
    let state = Rc::clone(state);
    row.connect_active_notify(move |row| {
        apply_setting(&state, key, SettingValue::Boolean(row.is_active()));
    });
    group.add(&row);
}

#[allow(clippy::too_many_arguments)]
fn add_spin(
    group: &adw::PreferencesGroup,
    state: &Rc<UiState>,
    key: SettingKey,
    title: &str,
    subtitle: &str,
    current: u32,
    minimum: u32,
    maximum: u32,
    step: u32,
) {
    let row = adw::SpinRow::with_range(f64::from(minimum), f64::from(maximum), f64::from(step));
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_value(f64::from(current));
    let state = Rc::clone(state);
    row.connect_value_notify(move |row| {
        let rounded = row.value().round();
        if (0.0..=f64::from(u32::MAX)).contains(&rounded) {
            apply_setting(&state, key, SettingValue::Integer(rounded as u32));
        }
    });
    group.add(&row);
}

fn add_text(
    group: &adw::PreferencesGroup,
    state: &Rc<UiState>,
    title: &str,
    subtitle: &str,
    current: &str,
    convert: impl Fn(&str) -> SettingValue + 'static,
    key: SettingKey,
) {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    let entry = gtk::Entry::builder()
        .text(current)
        .width_chars(24)
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&entry);
    row.set_activatable_widget(Some(&entry));
    let state = Rc::clone(state);
    entry.connect_changed(move |entry| {
        apply_setting(&state, key, convert(entry.text().as_str()));
    });
    group.add(&row);
}

fn add_alignment(group: &adw::PreferencesGroup, state: &Rc<UiState>, alignment: TitleAlignment) {
    let options = gtk::StringList::new(&["Left", "Center", "Right"]);
    let selected = match alignment {
        TitleAlignment::Left => 0,
        TitleAlignment::Center => 1,
        TitleAlignment::Right => 2,
    };
    let row = adw::ComboRow::builder()
        .title("Title alignment")
        .subtitle("Position within the space left by window buttons.")
        .model(&options)
        .selected(selected)
        .build();
    let state = Rc::clone(state);
    row.connect_selected_notify(move |row| {
        let value = match row.selected() {
            0 => "left",
            1 => "center",
            2 => "right",
            _ => return,
        };
        apply_setting(
            &state,
            SettingKey::TitleAlignment,
            SettingValue::Text(value.to_owned()),
        );
    });
    group.add(&row);
}

fn add_color(
    group: &adw::PreferencesGroup,
    state: &Rc<UiState>,
    key: SettingKey,
    title: &str,
    color: RgbColor,
) {
    let row = adw::ActionRow::builder().title(title).build();
    let dialog = gtk::ColorDialog::builder()
        .title(format!("Choose {title}"))
        .with_alpha(false)
        .build();
    let button = gtk::ColorDialogButton::builder()
        .dialog(&dialog)
        .rgba(&rgb_to_rgba(color))
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&button);
    row.set_activatable_widget(Some(&button));
    let state = Rc::clone(state);
    button.connect_rgba_notify(move |button| {
        apply_setting(&state, key, SettingValue::Text(rgba_to_hex(&button.rgba())));
    });
    group.add(&row);
}

fn apply_setting(state: &Rc<UiState>, key: SettingKey, value: SettingValue) {
    let source = buffer_text(&state.source);
    let result = SettingsDocument::parse(&source).and_then(|mut document| {
        document.set(key, value)?;
        Ok(document)
    });
    match result {
        Ok(document) => {
            let source = document.source();
            *state.document.borrow_mut() = document;
            state.synchronizing.set(true);
            state.source.set_text(&source);
            state.synchronizing.set(false);
            state.preview.queue_draw();
            show_status(state, "Unsaved changes", false);
        }
        Err(error) => show_error(state, &error.to_string()),
    }
}

fn save(state: &Rc<UiState>) -> Result<(), nobox_settings::SettingsError> {
    let source = buffer_text(&state.source);
    let document = SettingsDocument::parse(&source)?;
    document.save(&state.path)?;
    *state.saved_source.borrow_mut() = source;
    *state.document.borrow_mut() = document;
    show_status(
        state,
        "Saved. Choose Reconfigure from the nobox menu to apply it.",
        true,
    );
    Ok(())
}

fn buffer_text(buffer: &gtk::TextBuffer) -> String {
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
}

fn show_status(state: &UiState, message: &str, success: bool) {
    state.status.set_label(message);
    state.status.remove_css_class("error");
    state.status.remove_css_class("success");
    state.status.remove_css_class("dim-label");
    state
        .status
        .add_css_class(if success { "success" } else { "dim-label" });
}

fn show_error(state: &UiState, message: &str) {
    state.status.set_label(&format!("Not saved: {message}"));
    state.status.remove_css_class("dim-label");
    state.status.remove_css_class("success");
    state.status.add_css_class("error");
}

fn rgb_to_rgba(color: RgbColor) -> gdk::RGBA {
    let pixel = color.pixel();
    gdk::RGBA::new(
        f32::from(((pixel >> 16) & 0xff) as u8) / 255.0,
        f32::from(((pixel >> 8) & 0xff) as u8) / 255.0,
        f32::from((pixel & 0xff) as u8) / 255.0,
        1.0,
    )
}

fn rgba_to_hex(color: &gdk::RGBA) -> String {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.red()),
        channel(color.green()),
        channel(color.blue())
    )
}

fn install_preview(state: &Rc<UiState>) {
    let state = Rc::clone(state);
    let preview = state.preview.clone();
    preview.set_draw_func(move |_, context, width, height| {
        let Ok(config) = state.document.borrow().config() else {
            return;
        };
        let theme = config.theme;
        let width = f64::from(width);
        let height = f64::from(height);
        let border = f64::from(theme.border_width.max(1));
        set_source(context, theme.active_border);
        context.rectangle(8.0, 8.0, width - 16.0, height - 42.0);
        let _ = context.fill();
        set_source(context, theme.active_titlebar);
        context.rectangle(
            8.0 + border,
            8.0 + border,
            width - 16.0 - border * 2.0,
            f64::from(theme.titlebar_height.max(28)),
        );
        let _ = context.fill();
        set_source(context, theme.title_text);
        context.select_font_face(
            "monospace",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Bold,
        );
        context.set_font_size(13.0);
        context.move_to(22.0, 8.0 + border + 19.0);
        let _ = context.show_text("nobox — focused window");

        let button_size = 13.0;
        let mut x = width - 8.0 - border - 18.0;
        for color in [
            theme.close_button,
            theme.maximize_button,
            theme.minimize_button,
        ] {
            set_source(context, color);
            context.rectangle(x, 8.0 + border + 7.0, button_size, button_size);
            let _ = context.fill();
            x -= 21.0;
        }
        let strip_y = height - 24.0;
        for (offset, border_color, title_color) in [
            (0.0, theme.inactive_border, theme.inactive_titlebar),
            (
                width / 2.0 + 4.0,
                theme.urgent_border,
                theme.urgent_titlebar,
            ),
        ] {
            set_source(context, border_color);
            context.rectangle(offset + 8.0, strip_y, width / 2.0 - 20.0, 16.0);
            let _ = context.fill();
            set_source(context, title_color);
            context.rectangle(offset + 10.0, strip_y + 2.0, width / 2.0 - 24.0, 12.0);
            let _ = context.fill();
        }
    });
}

fn set_source(context: &gtk::cairo::Context, color: RgbColor) {
    let pixel = color.pixel();
    context.set_source_rgb(
        f64::from((pixel >> 16) & 0xff) / 255.0,
        f64::from((pixel >> 8) & 0xff) / 255.0,
        f64::from(pixel & 0xff) / 255.0,
    );
}
