//! Read-only direct-session discovery shared by diagnostics and W4 bring-up.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use rustix::fs::{Access, access};
use smithay::backend::udev::all_gpus;

use nobox_config::{
    MAX_OUTPUTS, OutputModeConfig, OutputPosition, OutputScale, OutputTransform, OutputsConfig,
};

use super::validate_runtime_dir;

const MAX_DEVICE_ENTRIES: usize = 256;
const MAX_CONNECTOR_MODES: usize = 256;

/// One physical mode reported by a connected desktop connector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectMode {
    /// Physical width in pixels.
    pub width: u32,
    /// Physical height in pixels.
    pub height: u32,
    /// Refresh in millihertz.
    pub refresh_millihz: u32,
    /// Whether the connector advertises this as preferred.
    pub preferred: bool,
}

/// Protocol-neutral connector inventory produced by the DRM scanner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectConnector {
    /// Stable connector name, such as `DP-1`.
    pub name: String,
    /// Modes reported by DRM in backend order.
    pub modes: Vec<DirectMode>,
}

/// One selected output in a complete candidate topology.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectOutputState {
    /// Stable connector name.
    pub name: String,
    /// Selected physical mode.
    pub mode: DirectMode,
    /// Logical desktop origin.
    pub position: OutputPosition,
    /// Logical transform.
    pub transform: OutputTransform,
    /// Exact fractional scale.
    pub scale: OutputScale,
    /// Whether this is the normalized primary output.
    pub primary: bool,
}

impl DirectOutputState {
    /// Returns the transformed logical size after exact ceiling division.
    #[must_use]
    pub fn logical_size(&self) -> (u32, u32) {
        let (width, height) = if matches!(
            self.transform,
            OutputTransform::Rotate90
                | OutputTransform::Rotate270
                | OutputTransform::Flipped90
                | OutputTransform::Flipped270
        ) {
            (self.mode.height, self.mode.width)
        } else {
            (self.mode.width, self.mode.height)
        };
        (
            scaled_logical_dimension(width, self.scale),
            scaled_logical_dimension(height, self.scale),
        )
    }
}

/// A complete, validated candidate topology ready for transactional apply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectTopology {
    /// Enabled connected outputs in deterministic discovery order.
    pub outputs: Vec<DirectOutputState>,
}

impl DirectTopology {
    /// Resolves configuration against connected desktop connectors without
    /// mutating DRM state. A caller applies this candidate in one transaction
    /// and retains its previous topology if application fails.
    ///
    /// # Errors
    ///
    /// Returns an error for hostile inventories, unavailable requested modes,
    /// coordinate overflow, or a candidate with no usable output.
    pub fn plan(
        config: &OutputsConfig,
        connectors: impl IntoIterator<Item = DirectConnector>,
    ) -> Result<Self, DirectTopologyError> {
        let connectors = connectors.into_iter().collect::<Vec<_>>();
        if connectors.len() > MAX_OUTPUTS {
            return Err(DirectTopologyError::TooManyConnectors(connectors.len()));
        }
        let mut names = std::collections::BTreeSet::new();
        let mut outputs = Vec::new();
        let mut automatic_x = 0_i32;
        for connector in connectors {
            if !valid_identifier(&connector.name) || connector.name.len() > 64 {
                return Err(DirectTopologyError::InvalidConnectorName(connector.name));
            }
            if !names.insert(connector.name.clone()) {
                return Err(DirectTopologyError::DuplicateConnector(connector.name));
            }
            if connector.modes.is_empty()
                || connector.modes.len() > MAX_CONNECTOR_MODES
                || connector
                    .modes
                    .iter()
                    .any(|mode| mode.width == 0 || mode.height == 0 || mode.refresh_millihz == 0)
            {
                return Err(DirectTopologyError::InvalidModeInventory(connector.name));
            }
            let rule = config.entry(&connector.name);
            if rule.is_some_and(|rule| !rule.enabled) {
                continue;
            }
            let requested = rule.and_then(|rule| rule.mode);
            let mode = select_mode(&connector.modes, requested).ok_or_else(|| {
                DirectTopologyError::RequestedModeUnavailable {
                    connector: connector.name.clone(),
                    requested,
                }
            })?;
            let transform = rule.map_or(OutputTransform::Normal, |rule| rule.transform);
            let scale = rule.map_or_else(OutputScale::default, |rule| rule.scale);
            let position = rule
                .and_then(|rule| rule.position)
                .unwrap_or(OutputPosition {
                    x: automatic_x,
                    y: 0,
                });
            let primary = rule.is_some_and(|rule| rule.primary);
            let output = DirectOutputState {
                name: connector.name,
                mode,
                position,
                transform,
                scale,
                primary,
            };
            let (logical_width, _) = output.logical_size();
            let right = position
                .x
                .checked_add(
                    i32::try_from(logical_width)
                        .map_err(|_| DirectTopologyError::GeometryOverflow)?,
                )
                .ok_or(DirectTopologyError::GeometryOverflow)?;
            automatic_x = automatic_x.max(right);
            outputs.push(output);
        }
        if outputs.is_empty() {
            return Err(DirectTopologyError::NoUsableOutput);
        }
        if !outputs.iter().any(|output| output.primary) {
            outputs[0].primary = true;
        }
        Ok(Self { outputs })
    }
}

/// Failure while deriving a candidate direct-output topology.
#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum DirectTopologyError {
    /// Bound scanner data before allocating backend objects.
    #[error("{0} connected outputs exceed the maximum of 32")]
    TooManyConnectors(usize),
    /// Scanner names must be portable config keys.
    #[error("DRM reported invalid connector name {0:?}")]
    InvalidConnectorName(String),
    /// Scanner connector identities must be unique.
    #[error("DRM reported connector {0:?} more than once")]
    DuplicateConnector(String),
    /// Mode inventories are non-empty and bounded.
    #[error("DRM reported an invalid mode inventory for {0:?}")]
    InvalidModeInventory(String),
    /// An exact configured mode was not reported by the connector.
    #[error("configured mode {requested:?} is unavailable on {connector:?}")]
    RequestedModeUnavailable {
        /// Connector whose modes did not match.
        connector: String,
        /// Exact requested mode.
        requested: Option<OutputModeConfig>,
    },
    /// At least one connected enabled desktop output must survive.
    #[error("candidate topology has no connected enabled desktop output")]
    NoUsableOutput,
    /// Logical layout arithmetic exceeded backend coordinates.
    #[error("candidate output topology exceeds logical coordinate bounds")]
    GeometryOverflow,
}

fn scaled_logical_dimension(physical: u32, scale: OutputScale) -> u32 {
    physical
        .saturating_mul(120)
        .saturating_add(u32::from(scale.units()).saturating_sub(1))
        / u32::from(scale.units())
}

fn select_mode(modes: &[DirectMode], requested: Option<OutputModeConfig>) -> Option<DirectMode> {
    let candidates = modes.iter().copied().filter(|mode| {
        mode.width > 0
            && mode.height > 0
            && mode.refresh_millihz > 0
            && requested.is_none_or(|requested| {
                mode.width == requested.width
                    && mode.height == requested.height
                    && requested
                        .refresh_millihz
                        .is_none_or(|refresh| refresh == mode.refresh_millihz)
            })
    });
    candidates.max_by_key(|mode| {
        (
            mode.preferred,
            u64::from(mode.width) * u64::from(mode.height),
            mode.refresh_millihz,
        )
    })
}

/// One device node observed without opening or claiming it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectDeviceDiagnostics {
    /// Absolute device-node path.
    pub path: PathBuf,
    /// Whether the current process may read and write the node according to
    /// the kernel's access check.
    pub accessible: bool,
}

/// Read-only environment diagnostics for a future direct Wayland session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectDiagnostics {
    /// Validated private runtime directory used for compositor sockets.
    pub runtime_directory: PathBuf,
    /// Seat selected from `XDG_SEAT`, defaulting to `seat0`.
    pub seat: String,
    /// Current logind session identity when one was exported.
    pub session_id: Option<String>,
    /// Current session type when one was exported.
    pub session_type: Option<String>,
    /// Explicit libseat backend, or `auto` when libseat will choose one.
    pub libseat_backend: String,
    /// DRM primary nodes discovered by udev for the selected seat.
    pub drm_devices: Vec<DirectDeviceDiagnostics>,
    /// DRM render nodes visible to the process.
    pub render_devices: Vec<DirectDeviceDiagnostics>,
    /// Input event nodes visible to udev/libinput.
    pub input_devices: Vec<DirectDeviceDiagnostics>,
    /// XWayland executable selected from `PATH`, when installed.
    pub xwayland: Option<PathBuf>,
}

impl DirectDiagnostics {
    /// Inspects direct-session prerequisites without opening libseat, DRM,
    /// input, or Wayland listening sockets.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime directory is unsafe, the seat name is
    /// hostile, or udev cannot enumerate DRM devices for the selected seat.
    pub fn inspect() -> Result<Self, DirectDiagnosticsError> {
        let runtime_directory = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or(DirectDiagnosticsError::MissingRuntimeDirectory)?;
        validate_runtime_dir(&runtime_directory)
            .map_err(|error| DirectDiagnosticsError::InvalidRuntimeDirectory(error.to_string()))?;
        let seat = env::var("XDG_SEAT").unwrap_or_else(|_| "seat0".to_owned());
        if !valid_identifier(&seat) {
            return Err(DirectDiagnosticsError::InvalidSeat(seat));
        }
        let session_id = optional_identifier("XDG_SESSION_ID")?;
        let session_type = optional_identifier("XDG_SESSION_TYPE")?;
        let libseat_backend = env::var("LIBSEAT_BACKEND").unwrap_or_else(|_| "auto".to_owned());
        if !valid_identifier(&libseat_backend) {
            return Err(DirectDiagnosticsError::InvalidLibseatBackend(
                libseat_backend,
            ));
        }

        let drm_devices = all_gpus(&seat)
            .map_err(DirectDiagnosticsError::Udev)?
            .into_iter()
            .take(MAX_DEVICE_ENTRIES)
            .map(device_diagnostics)
            .collect();
        let render_devices = device_entries(Path::new("/dev/dri"), "renderD")?;
        let input_devices = device_entries(Path::new("/dev/input"), "event")?;

        Ok(Self {
            runtime_directory,
            seat,
            session_id,
            session_type,
            libseat_backend,
            drm_devices,
            render_devices,
            input_devices,
            xwayland: find_executable("Xwayland"),
        })
    }

    /// Returns whether the read-only prerequisites are sufficient to attempt
    /// libseat acquisition in a real `--tty` run.
    #[must_use]
    pub fn ready(&self) -> bool {
        !self.drm_devices.is_empty()
            && self.drm_devices.iter().any(|device| device.accessible)
            && !self.render_devices.is_empty()
            && self.render_devices.iter().any(|device| device.accessible)
            && !self.input_devices.is_empty()
    }
}

/// Failure while inspecting direct-session prerequisites.
#[derive(Debug, thiserror::Error)]
pub enum DirectDiagnosticsError {
    /// `XDG_RUNTIME_DIR` was not exported.
    #[error("XDG_RUNTIME_DIR is unset")]
    MissingRuntimeDirectory,
    /// The runtime directory failed the shared ownership/mode checks.
    #[error("unsafe XDG_RUNTIME_DIR: {0}")]
    InvalidRuntimeDirectory(String),
    /// An exported identifier was not one bounded path-safe component.
    #[error("invalid {name} value {value:?}")]
    InvalidIdentifier {
        /// Environment variable name.
        name: &'static str,
        /// Rejected value.
        value: String,
    },
    /// The selected seat name was invalid.
    #[error("invalid XDG_SEAT value {0:?}")]
    InvalidSeat(String),
    /// The explicit libseat backend name was invalid.
    #[error("invalid LIBSEAT_BACKEND value {0:?}")]
    InvalidLibseatBackend(String),
    /// Udev could not enumerate GPUs for the selected seat.
    #[error("could not enumerate DRM devices: {0}")]
    Udev(#[source] io::Error),
    /// A device directory could not be enumerated.
    #[error("could not inspect device directory {path}: {source}")]
    DeviceDirectory {
        /// Directory being inspected.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
}

fn optional_identifier(name: &'static str) -> Result<Option<String>, DirectDiagnosticsError> {
    let Ok(value) = env::var(name) else {
        return Ok(None);
    };
    if valid_identifier(&value) {
        Ok(Some(value))
    } else {
        Err(DirectDiagnosticsError::InvalidIdentifier { name, value })
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn device_diagnostics(path: PathBuf) -> DirectDeviceDiagnostics {
    let accessible = access(&path, Access::READ_OK | Access::WRITE_OK).is_ok();
    DirectDeviceDiagnostics { path, accessible }
}

fn device_entries(
    directory: &Path,
    prefix: &str,
) -> Result<Vec<DirectDeviceDiagnostics>, DirectDiagnosticsError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(DirectDiagnosticsError::DeviceDirectory {
                path: directory.to_path_buf(),
                source,
            });
        }
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(prefix.as_bytes())
        })
        .map(|entry| entry.path())
        .take(MAX_DEVICE_ENTRIES)
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths.into_iter().map(device_diagnostics).collect())
}

fn find_executable(name: &str) -> Option<PathBuf> {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file() && access(path, Access::EXEC_OK).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, refresh_millihz: u32, preferred: bool) -> DirectMode {
        DirectMode {
            width,
            height,
            refresh_millihz,
            preferred,
        }
    }

    #[test]
    fn identifiers_are_bounded_path_components() {
        assert!(valid_identifier("seat0"));
        assert!(valid_identifier("seat-test_1.2"));
        assert!(!valid_identifier(""));
        assert!(!valid_identifier("../seat0"));
        assert!(!valid_identifier(&"x".repeat(129)));
    }

    #[test]
    fn missing_device_directories_are_empty_not_fatal() {
        let path = env::temp_dir().join(format!(
            "nobox-wayland-missing-devices-{}",
            std::process::id()
        ));
        assert_eq!(
            device_entries(&path, "event").expect("missing is valid"),
            []
        );
    }

    #[test]
    fn automatic_topology_uses_preferred_modes_and_logical_sizes() {
        let topology = DirectTopology::plan(
            &OutputsConfig::default(),
            [
                DirectConnector {
                    name: "eDP-1".to_owned(),
                    modes: vec![
                        mode(1280, 720, 60_000, false),
                        mode(1920, 1080, 60_000, true),
                    ],
                },
                DirectConnector {
                    name: "DP-1".to_owned(),
                    modes: vec![mode(2560, 1440, 144_000, true)],
                },
            ],
        )
        .expect("automatic topology");
        assert_eq!(topology.outputs.len(), 2);
        assert!(topology.outputs[0].primary);
        assert!(!topology.outputs[1].primary);
        assert_eq!(topology.outputs[0].logical_size(), (1920, 1080));
        assert_eq!(
            topology.outputs[1].position,
            OutputPosition { x: 1920, y: 0 }
        );
    }

    #[test]
    fn configured_topology_selects_exact_modes_transforms_and_scale() {
        let config = nobox_config::Config::parse(
            "[[outputs.entries]]\nname = 'eDP-1'\nenabled = false\n\
             [[outputs.entries]]\nname = 'DP-1'\nmode = '2560x1440@143.973'\n\
             position = { x = -1200, y = 40 }\ntransform = 'rotate90'\nscale = 1.2\nprimary = true",
        )
        .expect("valid config");
        let topology = DirectTopology::plan(
            &config.outputs,
            [
                DirectConnector {
                    name: "eDP-1".to_owned(),
                    modes: vec![mode(1920, 1080, 60_000, true)],
                },
                DirectConnector {
                    name: "DP-1".to_owned(),
                    modes: vec![
                        mode(2560, 1440, 60_000, false),
                        mode(2560, 1440, 143_973, false),
                    ],
                },
            ],
        )
        .expect("configured topology");
        assert_eq!(topology.outputs.len(), 1);
        let output = &topology.outputs[0];
        assert_eq!(output.mode.refresh_millihz, 143_973);
        assert_eq!(output.logical_size(), (1200, 2134));
        assert_eq!(output.position, OutputPosition { x: -1200, y: 40 });
        assert!(output.primary);
    }

    #[test]
    fn topology_failure_leaves_transaction_choice_to_the_caller() {
        let disabled =
            nobox_config::Config::parse("[[outputs.entries]]\nname = 'DP-1'\nenabled = false")
                .expect("valid config");
        let connector = DirectConnector {
            name: "DP-1".to_owned(),
            modes: vec![mode(1920, 1080, 60_000, true)],
        };
        assert_eq!(
            DirectTopology::plan(&disabled.outputs, [connector.clone()]),
            Err(DirectTopologyError::NoUsableOutput)
        );

        let unavailable = nobox_config::Config::parse(
            "[[outputs.entries]]\nname = 'DP-1'\nmode = '3840x2160@120'",
        )
        .expect("valid config");
        assert!(matches!(
            DirectTopology::plan(&unavailable.outputs, [connector]),
            Err(DirectTopologyError::RequestedModeUnavailable { .. })
        ));
    }
}
