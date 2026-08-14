//! Bounded process helpers shared by display backends.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Seek as _, SeekFrom},
    os::unix::fs::OpenOptionsExt as _,
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;

static OUTPUT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Failure while running a shell command under explicit time and output bounds.
#[derive(Debug, Error)]
pub enum BoundedCommandError {
    /// A private output file could not be allocated.
    #[error("could not allocate a private command-output file")]
    OutputAllocation,
    /// A process or file operation failed.
    #[error("could not {operation}: {source}")]
    Io {
        /// Operation that failed.
        operation: &'static str,
        /// Underlying operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// The child exceeded its deadline and was killed.
    #[error("command exceeded {0}ms")]
    Timeout(u128),
    /// The child exited unsuccessfully.
    #[error("command exited with {0}")]
    Exit(ExitStatus),
    /// The child wrote more bytes than the caller allowed.
    #[error("command output exceeded {0} bytes")]
    OutputTooLarge(usize),
    /// Output was not UTF-8 text.
    #[error("command output is not valid UTF-8")]
    InvalidUtf8,
}

struct PrivateOutput {
    path: PathBuf,
    file: File,
}

impl Drop for PrivateOutput {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Runs `/bin/sh -c` with null input/error, a deadline, and a UTF-8 output cap.
///
/// The child writes to a private unlinked-after-use file rather than a pipe, so
/// a full output buffer cannot deadlock deadline enforcement.
///
/// # Errors
///
/// Returns [`BoundedCommandError`] when setup, execution, the deadline, the
/// output bound, or UTF-8 validation fails.
pub fn bounded_shell_output(
    command: &str,
    timeout: Duration,
    max_bytes: usize,
) -> Result<String, BoundedCommandError> {
    let mut output = private_output()?;
    let child_output = output
        .file
        .try_clone()
        .map_err(|source| BoundedCommandError::Io {
            operation: "prepare command output",
            source,
        })?;
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::from(child_output))
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| BoundedCommandError::Io {
            operation: "start command",
            source,
        })?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(BoundedCommandError::Timeout(timeout.as_millis()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(BoundedCommandError::Io {
                    operation: "inspect command",
                    source,
                });
            }
        }
    };
    if !status.success() {
        return Err(BoundedCommandError::Exit(status));
    }
    output
        .file
        .seek(SeekFrom::Start(0))
        .map_err(|source| BoundedCommandError::Io {
            operation: "rewind command output",
            source,
        })?;
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1_024));
    output
        .file
        .by_ref()
        .take(
            u64::try_from(max_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|source| BoundedCommandError::Io {
            operation: "read command output",
            source,
        })?;
    if bytes.len() > max_bytes {
        return Err(BoundedCommandError::OutputTooLarge(max_bytes));
    }
    String::from_utf8(bytes).map_err(|_| BoundedCommandError::InvalidUtf8)
}

fn private_output() -> Result<PrivateOutput, BoundedCommandError> {
    let directory =
        std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    for _ in 0..16 {
        let sequence = OUTPUT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            "nobox-command-output-{}-{sequence}",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok(PrivateOutput { path, file }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(BoundedCommandError::Io {
                    operation: "create command output",
                    source,
                });
            }
        }
    }
    Err(BoundedCommandError::OutputAllocation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_output_enforces_success_size_utf8_and_deadline() {
        assert_eq!(
            bounded_shell_output("printf valid", Duration::from_secs(1), 16).unwrap(),
            "valid"
        );
        assert!(matches!(
            bounded_shell_output("printf toolong", Duration::from_secs(1), 3),
            Err(BoundedCommandError::OutputTooLarge(3))
        ));
        assert!(matches!(
            bounded_shell_output("printf '\\377'", Duration::from_secs(1), 16),
            Err(BoundedCommandError::InvalidUtf8)
        ));
        assert!(matches!(
            bounded_shell_output("sleep 1", Duration::from_millis(10), 16),
            Err(BoundedCommandError::Timeout(10))
        ));
    }
}
