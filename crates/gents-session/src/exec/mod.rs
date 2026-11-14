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

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedExecState {
    PendingSpawn,
    Running,
    Exited,
    KillSignaled,
    Killed,
    SpawnFailed,
    ReapFailed,
}

#[cfg(test)]
impl ManagedExecState {
    pub(crate) const ALL: [Self; 7] = [
        Self::PendingSpawn,
        Self::Running,
        Self::Exited,
        Self::KillSignaled,
        Self::Killed,
        Self::SpawnFailed,
        Self::ReapFailed,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PendingSpawn => "pendingSpawn",
            Self::Running => "running",
            Self::Exited => "exited",
            Self::KillSignaled => "killSignaled",
            Self::Killed => "killed",
            Self::SpawnFailed => "spawnFailed",
            Self::ReapFailed => "reapFailed",
        }
    }
}

#[cfg(windows)]
pub use process::FailedJobOwner;
