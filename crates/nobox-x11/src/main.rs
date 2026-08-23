//! X11 Nobox session executable.

mod xsmp;

use std::path::Path;

use anyhow::{Context, Result, bail};
use nobox_common::{
    Backend, BackendDriver, PanelSupervisor, launch_autostart, load_or_default,
    load_session_restore, parse_instance, replace_with_command, run_backend,
};
use nobox_config::state_path;
use nobox_runtime::{BackendCapabilities, RunDisposition};
use nobox_x11::{WindowManager, X11Diagnostics, running_instance};
use tracing::{info, warn};

struct X11Driver;

impl BackendDriver for X11Driver {
    const BACKEND: Backend = Backend::X11;
    const BINARY_NAME: &'static str = "nobox-x11";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    fn exit(display: Option<&str>, instance: Option<&str>) -> Result<()> {
        let running =
            running_instance(display).context("failed to locate the running X11 nobox instance")?;
        if let Some(requested) = instance {
            let requested = parse_instance(requested)?;
            if running.id() != &requested {
                bail!("the active X11 manager does not match --instance {requested}");
            }
        }
        running
            .sender()
            .context("failed to validate the running nobox instance")?
            .shutdown()
            .context("failed to request a clean nobox exit")
    }

    fn run(
        path: &Path,
        display: Option<&str>,
        no_autostart: bool,
        nested_x11: bool,
        tty: bool,
        sm_client_id: Option<&str>,
    ) -> Result<()> {
        if nested_x11 || tty {
            bail!("--nested-x11 and --tty are only valid with the Wayland backend");
        }
        run_x11(path, display, no_autostart, sm_client_id)
    }

    fn doctor(path: &Path, display: Option<&str>, nested_x11: bool, tty: bool) -> Result<()> {
        if nested_x11 || tty {
            bail!("--nested-x11 and --tty are only valid with the Wayland backend");
        }
        doctor(path, display)
    }
}

fn main() -> Result<()> {
    run_backend::<X11Driver>()
}

fn run_x11(
    path: &Path,
    display: Option<&str>,
    no_autostart: bool,
    requested_sm_client_id: Option<&str>,
) -> Result<()> {
    let session_path = state_path()?;
    let mut restore = load_session_restore(&session_path);
    let mut initial_start = true;
    let mut sm_client_id = requested_sm_client_id.map(str::to_owned);
    let panel = PanelSupervisor::new(path, display, BackendCapabilities::X11);

    loop {
        let config = load_or_default(path)?;
        panel.sync(&config);
        let mut wm = WindowManager::connect_with_session(display, config, restore)
            .context("failed to start the X11 backend")?;
        let control = wm
            .start_runtime_control(display)
            .context("failed to create the runtime control endpoint")?;
        let signals = nobox_common::SignalForwarder::install(control.clone())?;
        let xsmp = if xsmp::XsmpBridge::requested() {
            xsmp::XsmpBridge::connect(control, sm_client_id.as_deref())
        } else {
            None
        };
        if initial_start && !no_autostart {
            launch_autostart(path)?;
        }
        initial_start = false;

        let outcome = wm
            .run_with_session_coordination(
                || -> Result<_> {
                    let config = load_or_default(path)?;
                    panel.sync(&config);
                    Ok(config)
                },
                |snapshot| {
                    let success = match snapshot.save(&session_path) {
                        Ok(()) => true,
                        Err(error) => {
                            warn!(%error, path = %session_path.display(), "could not save requested XSMP snapshot");
                            false
                        }
                    };
                    if let Some(xsmp) = &xsmp {
                        xsmp.save_done(success);
                    }
                    success
                },
                || xsmp.as_ref().is_some_and(xsmp::XsmpBridge::request_logout),
            )
            .context("X11 event loop stopped")?;
        drop(signals);
        let (snapshot, disposition) = outcome.into_parts();
        if let Err(error) = snapshot.save(&session_path) {
            warn!(%error, path = %session_path.display(), "could not save session state");
        }
        if let Some(xsmp) = xsmp {
            sm_client_id = xsmp.client_id().or(sm_client_id);
            let reconnecting = matches!(disposition, RunDisposition::Restart { command: None });
            xsmp.close(!reconnecting);
        }
        panel.stop();
        match disposition {
            RunDisposition::Exit => return Ok(()),
            RunDisposition::Restart { command: None } => {
                info!("restarting nobox without rerunning autostart");
                restore = snapshot.into_restore();
            }
            RunDisposition::Restart {
                command: Some(command),
            } => return replace_with_command(&command),
        }
    }
}

fn doctor(path: &Path, display: Option<&str>) -> Result<()> {
    let mut errors = 0_u32;
    let mut warnings = 0_u32;
    let capabilities = BackendCapabilities::X11;
    println!(
        "[ok] backend capabilities: direct={}, session-restore={}, panel={}, agent-seat={}",
        capabilities.direct_session,
        capabilities.session_restore,
        capabilities.panel,
        capabilities.agent_seat
    );
    let config = if path.exists() {
        match nobox_config::Config::load(path) {
            Ok(config) => {
                println!("[ok] config: {}", path.display());
                config
            }
            Err(error) => {
                println!("[error] config: {}: {error}", path.display());
                errors = errors.saturating_add(1);
                nobox_config::Config::default()
            }
        }
    } else {
        println!(
            "[ok] config: built-in defaults ({} does not exist)",
            path.display()
        );
        nobox_config::Config::default()
    };

    let autostart = path
        .parent()
        .map_or_else(|| "autostart".into(), |parent| parent.join("autostart"));
    if autostart.is_file() {
        println!("[ok] autostart: {}", autostart.display());
    } else if autostart.exists() {
        println!(
            "[warn] autostart is not a regular file: {}",
            autostart.display()
        );
        warnings = warnings.saturating_add(1);
    } else {
        println!("[info] autostart: not installed ({})", autostart.display());
    }

    match state_path() {
        Ok(session) => match nobox_runtime::SessionSnapshot::load(&session) {
            Ok(_) if session.exists() => println!("[ok] session: {}", session.display()),
            Ok(_) => println!("[info] session: no saved state ({})", session.display()),
            Err(error) => {
                println!("[error] session: {error}");
                errors = errors.saturating_add(1);
            }
        },
        Err(error) => {
            println!("[error] session path: {error}");
            errors = errors.saturating_add(1);
        }
    }

    match X11Diagnostics::inspect(display, &config.theme.font) {
        Ok(diagnostics) => {
            print_x11_diagnostics(&diagnostics, &config.theme.font, &mut errors, &mut warnings);
        }
        Err(error) => {
            let selected = display.unwrap_or("$DISPLAY");
            println!("[error] X11 display {selected}: {error}");
            errors = errors.saturating_add(1);
        }
    }

    if errors == 0 {
        println!("ready: yes ({warnings} warning(s))");
        Ok(())
    } else {
        println!("ready: no ({errors} error(s), {warnings} warning(s))");
        bail!("nobox doctor found {errors} blocking issue(s)")
    }
}

fn print_x11_diagnostics(
    diagnostics: &X11Diagnostics,
    configured_font: &str,
    errors: &mut u32,
    warnings: &mut u32,
) {
    println!(
        "[ok] X11: {} release {}, protocol {}.{}, screen {} {}x{}x{}",
        diagnostics.vendor,
        diagnostics.release_number,
        diagnostics.protocol_version.0,
        diagnostics.protocol_version.1,
        diagnostics.screen_index,
        diagnostics.width,
        diagnostics.height,
        diagnostics.depth,
    );
    print_extension("RandR", diagnostics.randr_version, warnings);
    print_extension("Shape", diagnostics.shape_version, warnings);
    print_extension("Sync", diagnostics.sync_version, warnings);
    for (index, output) in diagnostics.outputs.iter().enumerate() {
        println!(
            "[ok] output {}: id={} {}x{}{:+}{:+}{}",
            index + 1,
            output.id.raw(),
            output.geometry.width,
            output.geometry.height,
            output.geometry.x,
            output.geometry.y,
            if output.primary { " primary" } else { "" },
        );
    }
    if diagnostics.configured_font_available {
        println!("[ok] X11 font: {configured_font}");
    } else if diagnostics.fallback_font_available {
        println!("[warn] X11 font is unavailable: {configured_font} (startup falls back to fixed)");
        *warnings = warnings.saturating_add(1);
    } else {
        println!("[error] X11 font is unavailable: {configured_font}");
        *errors = errors.saturating_add(1);
    }
    if let Some(owner) = diagnostics.window_manager_owner {
        println!(
            "[warn] another window manager owns this screen: {owner:#x} (exit it before starting nobox)"
        );
        *warnings = warnings.saturating_add(1);
    } else {
        println!("[ok] window-manager selection: available");
    }
}

fn print_extension<T>(name: &str, version: Option<(T, T)>, warnings: &mut u32)
where
    T: std::fmt::Display,
{
    if let Some((major, minor)) = version {
        println!("[ok] X11 extension {name}: {major}.{minor}");
    } else {
        println!("[warn] X11 extension {name}: unavailable; nobox will use its fallback");
        *warnings = warnings.saturating_add(1);
    }
}
