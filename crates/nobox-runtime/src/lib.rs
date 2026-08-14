//! Display-server-neutral process control, backend identity, and session handoff.

mod control;
mod process;
pub mod session;

pub use control::{
    BackendCapabilities, BackendKind, ControlError, ControlRequest, ControlSender, ControlServer,
    InstanceId, RunningInstance,
};
pub use process::{BoundedCommandError, bounded_shell_output};
pub use session::{RunDisposition, SessionError, SessionRestore, SessionSnapshot};
