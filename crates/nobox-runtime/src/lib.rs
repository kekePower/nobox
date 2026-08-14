//! Display-server-neutral process control, backend identity, and session handoff.

mod control;
pub mod session;

pub use control::{
    BackendCapabilities, BackendKind, ControlError, ControlRequest, ControlSender, ControlServer,
    InstanceId, RunningInstance,
};
pub use session::{RunDisposition, SessionError, SessionRestore, SessionSnapshot};
