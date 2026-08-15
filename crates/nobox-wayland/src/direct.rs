//! Read-only direct-session discovery shared by diagnostics and W4 bring-up.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use rustix::fs::{Access, access};
use smithay::backend::udev::all_gpus;

use super::validate_runtime_dir;

const MAX_DEVICE_ENTRIES: usize = 256;

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
}
