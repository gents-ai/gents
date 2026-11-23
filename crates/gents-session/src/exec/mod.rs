pub mod native_spawn;
mod output;
pub mod ownership;
mod process;
pub mod status;
#[cfg(test)]
mod tests;

pub use output::ManagedExecOutcome;
pub use ownership::{CancelProcessReply, ProcessStopOutcome};
pub(crate) use process::active_executor_snapshots;
pub use process::{
    run_managed_exec, spawn_managed_process, ManagedExecKind, ManagedExecRequest, ManagedProcess,
    SpawnManagedProcessRequest,
};

#[cfg(windows)]
pub use process::FailedJobOwner;
