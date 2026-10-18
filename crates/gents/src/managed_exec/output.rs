#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KillReport {
    pub(crate) pid: Option<i32>,
    pub(crate) term_signal_sent: bool,
    pub(crate) kill_signal_sent: bool,
    pub(crate) reaped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagedExecOutcome {
    Exited {
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_truncated: bool,
        stderr_truncated: bool,
    },
    TimedOut {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_truncated: bool,
        stderr_truncated: bool,
        kill: KillReport,
    },
    Cancelled {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_truncated: bool,
        stderr_truncated: bool,
        kill: KillReport,
    },
    SpawnFailed {
        error: String,
    },
}
