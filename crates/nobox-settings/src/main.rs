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
use nobox_config::{
    AgentPolicy, Config, MAX_WORKSPACES, PanelPosition, RgbColor, TitleAlignment, WorkspaceConfig,
    config_path,
};
use nobox_config::{ConfigDocument, SettingKey, SettingValue};

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
    document: RefCell<ConfigDocument>,
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
    let document = match ConfigDocument::load(&path) {
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
                if apply_workspace_count(&state, 6).is_none() {
                    eprintln!("nobox-settings: integration desktop-count update failed");
                    failed.set(true);
                    app.quit();
                    return;
                }
                apply_setting(
                    &state,
                    SettingKey::WorkspaceNames,
                    SettingValue::TextList(
                        ["main", "web", "chat", "media", "five", "six"]
                            .map(str::to_owned)
                            .to_vec(),
                    ),
                );
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
    document: ConfigDocument,
) -> Result<Rc<UiState>, nobox_config::ConfigDocumentError> {
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
        "Desktops",
    );
    stack.add_titled(
        &scroll_page(build_commands_page(&state, &config)),
        Some("commands"),
        "Commands",
    );
    stack.add_titled(
        &scroll_page(build_appearance_page(&state, &config)),
        Some("appearance"),
        "Appearance",
    );
    stack.add_titled(
        &scroll_page(build_panel_page(&state, &config)),
        Some("panel"),
        "Panel",
    );
    stack.add_titled(
        &scroll_page(build_agent_page(&state, &config)),
        Some("agent"),
        "Agent seat",
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

fn build_commands_page(state: &Rc<UiState>, config: &Config) -> gtk::Box {
    let page = page_box();
    let launchers = adw::PreferencesGroup::builder()
        .title("Standard commands")
        .description("Menus and shortcuts call these semantic actions, so changing a command updates every standard entry point.")
        .build();
    let terminal = add_text(
        &launchers,
        state,
        "Terminal",
        "Used by the root menu, Super+Enter, and the terminal shortcut below.",
        &config.commands.terminal,
        |value| SettingValue::Text(value.to_owned()),
        SettingKey::TerminalCommand,
    );
    terminal.add_css_class("monospace");
    let session = add_text(
        &launchers,
        state,
        "Session dialog",
        "Optional. When set, Log out launches this directly; for example, ssdd.",
        &config.commands.session,
        |value| SettingValue::Text(value.to_owned()),
        SettingKey::SessionCommand,
    );
    session.set_placeholder_text(Some("Built-in XSMP logout"));
    session.add_css_class("monospace");
    page.append(&launchers);

    let screenshots = adw::PreferencesGroup::builder()
        .title("Screenshots")
        .description("Keep separate commands because screenshot tools use different active-window arguments.")
        .build();
    let screen = add_text(
        &screenshots,
        state,
        "Full screen",
        "Command invoked for a whole-screen capture.",
        &config.commands.screenshot,
        |value| SettingValue::Text(value.to_owned()),
        SettingKey::ScreenshotCommand,
    );
    screen.add_css_class("monospace");
    let window = add_text(
        &screenshots,
        state,
        "Active window",
        "Command invoked for the focused window and its decoration.",
        &config.commands.window_screenshot,
        |value| SettingValue::Text(value.to_owned()),
        SettingKey::WindowScreenshotCommand,
    );
    window.add_css_class("monospace");
    page.append(&screenshots);

    let shortcuts = adw::PreferencesGroup::builder()
        .title("Common shortcuts")
        .description("Use C, A, S, and W for Control, Alt, Shift, and Super, followed by an X11 keysym name.")
        .build();
    for (title, subtitle, current, key) in [
        (
            "Terminal",
            "Additional traditional shortcut; Super+Enter remains available.",
            config.shortcuts.terminal.to_string(),
            SettingKey::TerminalShortcut,
        ),
        (
            "Full-screen screenshot",
            "Default: Print.",
            config.shortcuts.screenshot.to_string(),
            SettingKey::ScreenshotShortcut,
        ),
        (
            "Active-window screenshot",
            "Default: Alt+Print.",
            config.shortcuts.window_screenshot.to_string(),
            SettingKey::WindowScreenshotShortcut,
        ),
    ] {
        let entry = add_text(
            &shortcuts,
            state,
            title,
            subtitle,
            &current,
            |value| SettingValue::Text(value.to_owned()),
            key,
        );
        entry.add_css_class("monospace");
    }
    page.append(&shortcuts);
    page
}

fn build_workspace_page(state: &Rc<UiState>, config: &Config) -> gtk::Box {
    let page = page_box();
    let layout = adw::PreferencesGroup::builder()
        .title("Desktop layout")
        .description("Four desktops match the Openbox default. Arrange them as one row or a directional grid.")
        .build();
    let count_row = adw::SpinRow::with_range(
        1.0,
        f64::from(u32::try_from(MAX_WORKSPACES).unwrap_or(u32::MAX)),
        1.0,
    );
    count_row.set_title("Number of desktops");
    count_row.set_subtitle("Add or remove desktops while keeping existing names in order.");
    count_row.set_value(config.workspaces.names.len() as f64);
    layout.add(&count_row);
    let columns_row = add_spin(
        &layout,
        state,
        SettingKey::WorkspaceColumns,
        "Grid columns",
        "Use zero for one row; otherwise choose a fixed column count.",
        config.workspaces.columns,
        0,
        u32::try_from(config.workspaces.names.len()).unwrap_or(u32::MAX),
        1,
    );
    add_switch(
        &layout,
        state,
        SettingKey::WorkspaceWrap,
        "Default wrap policy",
        "Custom directional actions inherit this unless they set wrap explicitly.",
        config.workspaces.wrap,
    );
    let initial_row = add_spin(
        &layout,
        state,
        SettingKey::InitialWorkspace,
        "Initial desktop",
        "Used for a new session; saved session state takes precedence.",
        config.workspaces.initial,
        1,
        u32::try_from(config.workspaces.names.len()).unwrap_or(32),
        1,
    );
    page.append(&layout);

    let names = adw::PreferencesGroup::builder()
        .title("Desktop names")
        .description("Names appear in the panel, menus, and desktop-switching interfaces.")
        .build();
    let controls = Rc::new(
        (0..MAX_WORKSPACES)
            .map(|index| {
                let row = adw::ActionRow::builder()
                    .title(format!("Desktop {}", index.saturating_add(1)))
                    .visible(index < config.workspaces.names.len())
                    .build();
                let entry = gtk::Entry::builder()
                    .text(
                        config
                            .workspaces
                            .names
                            .get(index)
                            .map_or_else(|| index.saturating_add(1).to_string(), Clone::clone),
                    )
                    .width_chars(24)
                    .valign(gtk::Align::Center)
                    .build();
                row.add_suffix(&entry);
                row.set_activatable_widget(Some(&entry));
                names.add(&row);
                (row, entry)
            })
            .collect::<Vec<_>>(),
    );
    let visible_count = Rc::new(Cell::new(config.workspaces.names.len()));
    for index in 0..controls.len() {
        let entry = controls[index].1.clone();
        let controls = Rc::clone(&controls);
        let visible_count = Rc::clone(&visible_count);
        let state = Rc::clone(state);
        entry.connect_changed(move |_| {
            if state.synchronizing.get() {
                return;
            }
            let names = controls
                .iter()
                .take(visible_count.get())
                .map(|(_, entry)| entry.text().trim().to_owned())
                .collect();
            apply_setting(
                &state,
                SettingKey::WorkspaceNames,
                SettingValue::TextList(names),
            );
        });
    }
    let controls_for_count = Rc::clone(&controls);
    let visible_count_for_count = Rc::clone(&visible_count);
    let state_for_count = Rc::clone(state);
    count_row.connect_value_notify(move |row| {
        if state_for_count.synchronizing.get() {
            return;
        }
        let rounded = row.value().round();
        if !(1.0..=MAX_WORKSPACES as f64).contains(&rounded) {
            return;
        }
        let count = rounded as u32;
        let Some(workspace) = apply_workspace_count(&state_for_count, count) else {
            state_for_count.synchronizing.set(true);
            row.set_value(visible_count_for_count.get() as f64);
            state_for_count.synchronizing.set(false);
            return;
        };
        let count = workspace.names.len();
        visible_count_for_count.set(count);
        state_for_count.synchronizing.set(true);
        for (index, (name_row, entry)) in controls_for_count.iter().enumerate() {
            name_row.set_visible(index < count);
            entry.set_text(
                workspace
                    .names
                    .get(index)
                    .map_or_else(|| index.saturating_add(1).to_string(), Clone::clone)
                    .as_str(),
            );
        }
        columns_row.adjustment().set_upper(count as f64);
        columns_row.set_value(f64::from(workspace.columns));
        initial_row.adjustment().set_upper(count as f64);
        initial_row.set_value(f64::from(workspace.initial));
        state_for_count.synchronizing.set(false);
    });
    page.append(&names);

    let margins = adw::PreferencesGroup::builder()
        .title("Reserved screen edges")
        .description("Keep windows away from outer screen edges independently of panels.")
        .build();
    for (key, title, value) in [
        (SettingKey::MarginTop, "Top", config.margins.top),
        (SettingKey::MarginRight, "Right", config.margins.right),
        (SettingKey::MarginBottom, "Bottom", config.margins.bottom),
        (SettingKey::MarginLeft, "Left", config.margins.left),
    ] {
        add_spin(
            &margins,
            state,
            key,
            title,
            "Reserved pixels.",
            value,
            0,
            16_384,
            1,
        );
    }
    page.append(&margins);

    let explanation = adw::PreferencesGroup::builder()
        .title("What stays stable")
        .description("Changing the count keeps existing windows on a valid desktop. Removing a desktop merges its clients into the final remaining desktop.")
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

fn build_panel_page(state: &Rc<UiState>, config: &Config) -> gtk::Box {
    let page = page_box();
    let panel = adw::PreferencesGroup::builder()
        .title("Optional panel")
        .description("A separate lightweight process inspired by Tint2. The window manager remains independent if it is disabled or unavailable.")
        .build();
    add_switch(
        &panel,
        state,
        SettingKey::PanelEnabled,
        "Enable the panel",
        "Start nobox-panel with the desktop session.",
        config.panel.enabled,
    );
    add_panel_position(&panel, state, config.panel.position);
    add_spin(
        &panel,
        state,
        SettingKey::PanelHeight,
        "Panel height",
        "Reserved edge height in pixels.",
        config.panel.height,
        20,
        96,
        1,
    );
    add_switch(
        &panel,
        state,
        SettingKey::PanelShowWorkspaces,
        "Workspace buttons",
        "Switch directly between configured workspaces.",
        config.panel.show_workspaces,
    );
    add_switch(
        &panel,
        state,
        SettingKey::PanelShowTasks,
        "Task buttons",
        "Show windows from the current workspace.",
        config.panel.show_tasks,
    );
    add_switch(
        &panel,
        state,
        SettingKey::PanelShowClock,
        "Clock",
        "Show local time at the trailing edge.",
        config.panel.show_clock,
    );
    page.append(&panel);

    let colors = adw::PreferencesGroup::builder().title("Colors").build();
    add_color(
        &colors,
        state,
        SettingKey::PanelBackground,
        "Background",
        config.panel.background,
    );
    page.append(&colors);
    page
}

fn add_panel_position(group: &adw::PreferencesGroup, state: &Rc<UiState>, position: PanelPosition) {
    let options = gtk::StringList::new(&["Top", "Bottom"]);
    let selected = match position {
        PanelPosition::Top => 0,
        PanelPosition::Bottom => 1,
    };
    let row = adw::ComboRow::builder()
        .title("Position")
        .subtitle("Screen edge occupied by the panel.")
        .model(&options)
        .selected(selected)
        .build();
    let state = Rc::clone(state);
    row.connect_selected_notify(move |row| {
        let value = match row.selected() {
            0 => "top",
            1 => "bottom",
            _ => return,
        };
        apply_setting(
            &state,
            SettingKey::PanelPosition,
            SettingValue::Text(value.to_owned()),
        );
    });
    group.add(&row);
}

/// The agent seat: whether one exists, how it behaves, and who holds a grant.
fn build_agent_page(state: &Rc<UiState>, config: &Config) -> gtk::Box {
    let page = page_box();
    let seat = adw::PreferencesGroup::builder()
        .title("Agent seat")
        .description(
            "Lets an AI agent harness observe and act on this desktop through the window \
             manager, with capabilities you grant. Nothing is exposed until you enable this, \
             and a harness holds only what a grant below names.",
        )
        .build();
    add_switch(
        &seat,
        state,
        SettingKey::AgentEnabled,
        "Offer an agent seat",
        "Listen for agent companions on a private socket.",
        config.agent.enabled,
    );
    add_agent_policy(&seat, state, config.agent.policy);
    add_spin(
        &seat,
        state,
        SettingKey::AgentSuppressionMs,
        "Your input wins for",
        "Milliseconds after you type or click during which agent input is refused.",
        config.agent.suppression_ms,
        0,
        60_000,
        50,
    );
    add_text(
        &seat,
        state,
        "Kill chord",
        "Freezes every agent session at once, and resumes them when pressed again.",
        &config.agent.kill_chord.to_string(),
        |text| SettingValue::Text(text.to_owned()),
        SettingKey::AgentKillChord,
    );
    page.append(&seat);

    let grants = adw::PreferencesGroup::builder()
        .title("Granted companions")
        .description(
            "A grant binds to a program's path, never to a name it gives for itself. Remove \
             one to take its capabilities back; running sessions lose them at the next \
             reconfigure.",
        )
        .build();
    let rows: Rc<RefCell<Vec<gtk::Widget>>> = Rc::new(RefCell::new(Vec::new()));
    populate_grants(&grants, state, &rows);
    page.append(&grants);

    let privacy = adw::PreferencesGroup::builder()
        .title("Windows kept private")
        .description(
            "Application rules can hide a window from agents entirely, or keep its title \
             private. Edit them under Advanced TOML with agent_visibility.",
        )
        .build();
    let hidden: Vec<String> = config
        .applications
        .iter()
        .filter_map(|rule| {
            let visibility = rule.settings.agent_visibility?;
            if matches!(visibility, nobox_config::AgentVisibility::Visible) {
                return None;
            }
            let matcher = &rule.matcher;
            let subject = matcher
                .class
                .clone()
                .or_else(|| matcher.name.clone())
                .or_else(|| matcher.title.clone())
                .unwrap_or_else(|| "matching windows".to_owned());
            Some(format!("{subject}: {visibility:?}").to_lowercase())
        })
        .collect();
    if hidden.is_empty() {
        privacy.add(
            &adw::ActionRow::builder()
                .title("No windows are hidden from agents")
                .subtitle("Every window a grant covers can be seen by a session holding it.")
                .build(),
        );
    } else {
        for entry in hidden {
            privacy.add(&adw::ActionRow::builder().title(&entry).build());
        }
    }
    page.append(&privacy);
    page
}

/// Rebuilds the grant list from the document currently being edited.
fn populate_grants(
    group: &adw::PreferencesGroup,
    state: &Rc<UiState>,
    rows: &Rc<RefCell<Vec<gtk::Widget>>>,
) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }
    let Ok(config) = ConfigDocument::parse(&buffer_text(&state.source)).and_then(|document| {
        let config = document.config()?;
        Ok(config)
    }) else {
        return;
    };
    if config.agent.grants.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No companion holds a grant")
            .subtitle(
                "With \"Ask\" selected above, the next companion that connects raises a \
                 dialog you can answer.",
            )
            .build();
        group.add(&row);
        rows.borrow_mut().push(row.upcast());
        return;
    }
    for (index, grant) in config.agent.grants.iter().enumerate() {
        let capabilities = grant
            .capabilities
            .iter()
            .map(|capability| capability.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let scope = if grant.scope.is_some() {
            " (limited to matching windows)"
        } else {
            ""
        };
        let title = if grant.label.is_empty() {
            grant.executable.display().to_string()
        } else {
            grant.label.clone()
        };
        let row = adw::ActionRow::builder()
            .title(&title)
            .subtitle(format!(
                "{}\n{capabilities}{scope}",
                grant.executable.display()
            ))
            .build();
        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Remove this grant")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let state_remove = Rc::clone(state);
        let group_remove = group.clone();
        let rows_remove = Rc::clone(rows);
        remove.connect_clicked(move |_| {
            remove_grant(&state_remove, index);
            populate_grants(&group_remove, &state_remove, &rows_remove);
        });
        row.add_suffix(&remove);
        group.add(&row);
        rows.borrow_mut().push(row.upcast());
    }
}

fn remove_grant(state: &Rc<UiState>, index: usize) {
    let source = buffer_text(&state.source);
    let result = ConfigDocument::parse(&source).and_then(|mut document| {
        document.remove_agent_grant(index)?;
        Ok(document)
    });
    match result {
        Ok(document) => accept_document(state, document),
        Err(error) => show_error(state, &error.to_string()),
    }
}

fn add_agent_policy(group: &adw::PreferencesGroup, state: &Rc<UiState>, policy: AgentPolicy) {
    let options = gtk::StringList::new(&["Deny", "Ask"]);
    let selected = match policy {
        AgentPolicy::Deny => 0,
        AgentPolicy::Ask => 1,
    };
    let row = adw::ComboRow::builder()
        .title("Companions with no grant")
        .subtitle("Refuse them outright, or ask you with a dialog the window manager draws.")
        .model(&options)
        .selected(selected)
        .build();
    let state = Rc::clone(state);
    row.connect_selected_notify(move |row| {
        let value = match row.selected() {
            0 => "deny",
            1 => "ask",
            _ => return,
        };
        apply_setting(
            &state,
            SettingKey::AgentPolicy,
            SettingValue::Text(value.to_owned()),
        );
    });
    group.add(&row);
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
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(f64::from(minimum), f64::from(maximum), f64::from(step));
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_value(f64::from(current));
    let state = Rc::clone(state);
    row.connect_value_notify(move |row| {
        if state.synchronizing.get() {
            return;
        }
        let rounded = row.value().round();
        if (0.0..=f64::from(u32::MAX)).contains(&rounded) {
            apply_setting(&state, key, SettingValue::Integer(rounded as u32));
        }
    });
    group.add(&row);
    row
}

fn add_text(
    group: &adw::PreferencesGroup,
    state: &Rc<UiState>,
    title: &str,
    subtitle: &str,
    current: &str,
    convert: impl Fn(&str) -> SettingValue + 'static,
    key: SettingKey,
) -> gtk::Entry {
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
    entry
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
    let result = ConfigDocument::parse(&source).and_then(|mut document| {
        document.set(key, value)?;
        Ok(document)
    });
    match result {
        Ok(document) => accept_document(state, document),
        Err(error) => show_error(state, &error.to_string()),
    }
}

fn apply_workspace_count(state: &Rc<UiState>, count: u32) -> Option<WorkspaceConfig> {
    let source = buffer_text(&state.source);
    let result = ConfigDocument::parse(&source).and_then(|mut document| {
        document.set_workspace_count(count)?;
        let workspace = document.config()?.workspaces;
        Ok((document, workspace))
    });
    match result {
        Ok((document, workspace)) => {
            accept_document(state, document);
            Some(workspace)
        }
        Err(error) => {
            show_error(state, &error.to_string());
            None
        }
    }
}

fn accept_document(state: &Rc<UiState>, document: ConfigDocument) {
    let source = document.source();
    *state.document.borrow_mut() = document;
    state.synchronizing.set(true);
    state.source.set_text(&source);
    state.synchronizing.set(false);
    state.preview.queue_draw();
    show_status(state, "Unsaved changes", false);
}

fn save(state: &Rc<UiState>) -> Result<(), nobox_config::ConfigDocumentError> {
    let source = buffer_text(&state.source);
    let document = ConfigDocument::parse(&source)?;
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
