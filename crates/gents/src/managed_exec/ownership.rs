//! Host process ownership for managed background executions (#1858).
//!
//! Lean: `ManagedExec.Ownership`. A managed child leads its own session and
//! process group, so it survives the runtime. After a runtime loss the only
//! ownership evidence is a durable host record of the leader pid, its group
//! and its OS start identity, kept beside the runtime's exclusively locked
//! store. A bare pid, or a group whose recorded leader is gone, proves
//! nothing: the OS reuses both ids.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Lean `ManagedExec.ProcessObservation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessObservation {
    Running,
    Exited,
    Unowned,
}

impl ProcessObservation {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 3] = [Self::Running, Self::Exited, Self::Unowned];

    #[cfg(test)]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Unowned => "unowned",
        }
    }

    /// Lean `ManagedExec.mayTerminate`.
    pub(crate) const fn may_terminate(self) -> bool {
        matches!(self, Self::Running)
    }
}

/// Lean `ManagedExec.StopOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStopOutcome {
    Stopped,
    AlreadyExited,
    StillRunning,
    NotOwned,
}

impl ProcessStopOutcome {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 4] = [
        Self::Stopped,
        Self::AlreadyExited,
        Self::StillRunning,
        Self::NotOwned,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::AlreadyExited => "alreadyExited",
            Self::StillRunning => "stillRunning",
            Self::NotOwned => "notOwned",
        }
    }

    /// Lean `ManagedExec.stopOutcome`.
    pub(crate) const fn from_observations(
        before: ProcessObservation,
        after: ProcessObservation,
    ) -> Self {
        match (before, after) {
            (ProcessObservation::Running, ProcessObservation::Exited) => Self::Stopped,
            (ProcessObservation::Running, _) => Self::StillRunning,
            (ProcessObservation::Exited, _) => Self::AlreadyExited,
            (ProcessObservation::Unowned, _) => Self::NotOwned,
        }
    }

    /// Lean `ManagedExec.cancelReply`.
    pub const fn cancel_reply(self) -> CancelProcessReply {
        match self {
            Self::Stopped => CancelProcessReply::Cancelled,
            Self::AlreadyExited | Self::NotOwned => CancelProcessReply::Lost,
            Self::StillRunning => CancelProcessReply::Unverified,
        }
    }
}

/// Lean `ManagedExec.CancelReply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelProcessReply {
    Cancelled,
    Lost,
    Unverified,
}

impl CancelProcessReply {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 3] = [Self::Cancelled, Self::Lost, Self::Unverified];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Lost => "lost",
            Self::Unverified => "unverified",
        }
    }
}

/// Leader pid, its process group, and the OS start identity that
/// distinguishes it from a later process reusing the same pid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: i32,
    pub(crate) pgid: i32,
    pub(crate) start: String,
}

impl ProcessIdentity {
    /// Identity of a live, non-zombie process, or `None`.
    pub(crate) fn of_live(pid: i32) -> Option<Self> {
        let info = platform::process_info(pid)?;
        (!info.zombie).then_some(Self {
            pid,
            pgid: info.pgid,
            start: info.start,
        })
    }

    /// Ownership probe. `Running` requires the recorded leader to be alive
    /// with the recorded start identity and group; a leaderless group with
    /// live members is `Unowned` because its id may have been reused.
    pub(crate) fn observe(&self) -> ProcessObservation {
        if self.pid <= 0 || self.pgid <= 0 {
            return ProcessObservation::Unowned;
        }
        match platform::process_info(self.pid) {
            Some(info) if !info.zombie => {
                if info.start == self.start && info.pgid == self.pgid {
                    ProcessObservation::Running
                } else {
                    ProcessObservation::Unowned
                }
            }
            _ => match platform::group_has_live_member(self.pgid) {
                Some(false) => ProcessObservation::Exited,
                Some(true) | None => ProcessObservation::Unowned,
            },
        }
    }

    /// Group observation after this owner signalled a group it had proven
    /// owned. A member that is not a zombie keeps the group running.
    fn observe_signalled_group(&self) -> ProcessObservation {
        match platform::group_has_live_member(self.pgid) {
            Some(false) => ProcessObservation::Exited,
            Some(true) | None => ProcessObservation::Running,
        }
    }

    /// Waits for a group this owner already signalled to empty.
    pub(crate) async fn await_signalled_group_exit(&self, wait: Duration) -> ProcessObservation {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let observed = self.observe_signalled_group();
            if observed == ProcessObservation::Exited || tokio::time::Instant::now() >= deadline {
                return observed;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Waits for the recorded leader to stop being a proven-owned running
    /// process.
    pub(crate) async fn await_leader_exit(&self, wait: Duration) -> ProcessObservation {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let observed = self.observe();
            if observed != ProcessObservation::Running || tokio::time::Instant::now() >= deadline {
                return observed;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Stops a proven-owned group: TERM, then KILL, then observe. Returns the
    /// observations before and after; nothing is signalled unless `before`
    /// admits termination.
    pub(crate) async fn stop(&self) -> (ProcessObservation, ProcessObservation) {
        let before = self.observe();
        if !before.may_terminate() {
            return (before, before);
        }
        platform::signal_group(self.pgid, libc_signal::TERM);
        let after_term = self.await_signalled_group_exit(TERM_GRACE).await;
        if after_term == ProcessObservation::Exited {
            return (before, after_term);
        }
        platform::signal_group(self.pgid, libc_signal::KILL);
        (before, self.await_signalled_group_exit(KILL_GRACE).await)
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(25);
const TERM_GRACE: Duration = Duration::from_millis(500);
pub(crate) const KILL_GRACE: Duration = Duration::from_secs(2);

/// Durable host record binding one background execution to its process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProcessRecord {
    pub(crate) tool_call_id: String,
    pub(crate) tool_call_doc_id: String,
    pub(crate) identity: ProcessIdentity,
}

/// Durable records live in a directory of the runtime's exclusively locked
/// store, so only that store's runtime reads them after a restart. Without a
/// directory, records are volatile and a restart finds none.
#[derive(Debug, Clone)]
pub(crate) enum ProcessRecordStore {
    Volatile(Arc<std::sync::Mutex<std::collections::HashMap<String, ProcessRecord>>>),
    Durable(PathBuf),
}

impl Default for ProcessRecordStore {
    fn default() -> Self {
        Self::Volatile(Arc::default())
    }
}

impl ProcessRecordStore {
    fn path_for(dir: &std::path::Path, tool_call_id: &str) -> PathBuf {
        let name = tool_call_id
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        dir.join(format!("{name}.json"))
    }

    fn volatile(
        records: &std::sync::Mutex<std::collections::HashMap<String, ProcessRecord>>,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, ProcessRecord>> {
        records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn write(&self, record: &ProcessRecord) -> std::io::Result<()> {
        let dir = match self {
            Self::Volatile(records) => {
                Self::volatile(records).insert(record.tool_call_id.clone(), record.clone());
                return Ok(());
            }
            Self::Durable(dir) => dir,
        };
        std::fs::create_dir_all(dir)?;
        let path = Self::path_for(dir, &record.tool_call_id);
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&temporary, &path)?;
        // The record must survive the crash it exists for.
        std::fs::File::open(dir).and_then(|dir| dir.sync_all())
    }

    pub(crate) fn read(&self, tool_call_id: &str) -> Option<ProcessRecord> {
        let dir = match self {
            Self::Volatile(records) => return Self::volatile(records).get(tool_call_id).cloned(),
            Self::Durable(dir) => dir,
        };
        let bytes = std::fs::read(Self::path_for(dir, tool_call_id)).ok()?;
        match serde_json::from_slice::<ProcessRecord>(&bytes) {
            Ok(record) if record.tool_call_id == tool_call_id => Some(record),
            Ok(_) | Err(_) => {
                tracing::warn!(
                    tool_call_id,
                    "ignoring unreadable background process record"
                );
                None
            }
        }
    }

    pub(crate) fn remove(&self, tool_call_id: &str) {
        let dir = match self {
            Self::Volatile(records) => {
                Self::volatile(records).remove(tool_call_id);
                return;
            }
            Self::Durable(dir) => dir,
        };
        match std::fs::remove_file(Self::path_for(dir, tool_call_id)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(tool_call_id, %error, "failed to remove background process record");
            }
        }
    }

    pub(crate) fn list(&self) -> Vec<ProcessRecord> {
        let dir = match self {
            Self::Volatile(records) => return Self::volatile(records).values().cloned().collect(),
            Self::Durable(dir) => dir,
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .filter_map(|path| std::fs::read(path).ok())
            .filter_map(|bytes| serde_json::from_slice::<ProcessRecord>(&bytes).ok())
            .collect()
    }
}

type RecordFn = dyn Fn(ProcessIdentity) + Send + Sync;

/// Task-scoped sink for the identity of each process a managed execution
/// spawns. Only background executions install one.
#[derive(Clone)]
pub(crate) struct ProcessRecorder(Arc<RecordFn>);

impl ProcessRecorder {
    pub(crate) fn new(record: impl Fn(ProcessIdentity) + Send + Sync + 'static) -> Self {
        Self(Arc::new(record))
    }
}

tokio::task_local! {
    static PROCESS_RECORDER: ProcessRecorder;
}

pub(crate) async fn scope_process_recorder<F>(recorder: ProcessRecorder, future: F) -> F::Output
where
    F: std::future::Future,
{
    PROCESS_RECORDER.scope(recorder, future).await
}

/// Called by the spawner right after a child starts, before it is awaited.
pub(super) fn record_spawned(pid: i32) {
    let Ok(recorder) = PROCESS_RECORDER.try_with(Clone::clone) else {
        return;
    };
    match ProcessIdentity::of_live(pid) {
        Some(identity) => (recorder.0)(identity),
        None => tracing::warn!(
            pid,
            "spawned background process exited before it was recorded"
        ),
    }
}

mod libc_signal {
    #[cfg(unix)]
    pub(super) const TERM: i32 = libc::SIGTERM;
    #[cfg(unix)]
    pub(super) const KILL: i32 = libc::SIGKILL;
    #[cfg(not(unix))]
    pub(super) const TERM: i32 = 15;
    #[cfg(not(unix))]
    pub(super) const KILL: i32 = 9;
}

struct ProcessInfo {
    pgid: i32,
    start: String,
    zombie: bool,
}

#[cfg(target_os = "macos")]
mod platform {
    use super::ProcessInfo;

    // <libproc.h> PROC_PGRP_ONLY; not exported by the libc crate.
    const PROC_PGRP_ONLY: u32 = 2;

    pub(super) fn process_info(pid: i32) -> Option<ProcessInfo> {
        if pid <= 0 {
            return None;
        }
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: the buffer is a properly sized, writable proc_bsdinfo.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: proc_pidinfo filled the whole structure.
        let info = unsafe { info.assume_init() };
        Some(ProcessInfo {
            pgid: i32::try_from(info.pbi_pgid).ok()?,
            start: format!(
                "darwin:{}.{:06}",
                info.pbi_start_tvsec, info.pbi_start_tvusec
            ),
            zombie: info.pbi_status == libc::SZOMB,
        })
    }

    pub(super) fn group_has_live_member(pgid: i32) -> Option<bool> {
        let pgid = u32::try_from(pgid).ok()?;
        let mut capacity = 64usize;
        loop {
            let mut pids = vec![0 as libc::pid_t; capacity];
            let bytes = (capacity * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
            // SAFETY: the buffer holds `capacity` pid_t values.
            let written = unsafe {
                libc::proc_listpids(PROC_PGRP_ONLY, pgid, pids.as_mut_ptr().cast(), bytes)
            };
            if written < 0 {
                return None;
            }
            let count = written as usize / std::mem::size_of::<libc::pid_t>();
            if count >= capacity {
                capacity *= 4;
                continue;
            }
            return Some(pids[..count].iter().any(|pid| {
                process_info(*pid).is_some_and(|info| !info.zombie && info.pgid as u32 == pgid)
            }));
        }
    }

    pub(super) fn signal_group(pgid: i32, signal: i32) {
        if pgid > 0 {
            // SAFETY: plain kill(2) on a process group this owner proved.
            let _ = unsafe { libc::kill(-pgid, signal) };
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::ProcessInfo;

    struct Stat {
        state: char,
        pgrp: i32,
        start_ticks: u64,
    }

    fn stat(pid: i32) -> Option<Stat> {
        let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name may contain spaces and parentheses; fields resume
        // after its last ')'.
        let rest = raw.get(raw.rfind(')')? + 2..)?;
        let fields = rest.split_whitespace().collect::<Vec<_>>();
        Some(Stat {
            state: fields.first()?.chars().next()?,
            pgrp: fields.get(2)?.parse().ok()?,
            start_ticks: fields.get(19)?.parse().ok()?,
        })
    }

    fn boot_id() -> String {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map(|id| id.trim().to_owned())
            .unwrap_or_default()
    }

    pub(super) fn process_info(pid: i32) -> Option<ProcessInfo> {
        if pid <= 0 {
            return None;
        }
        let stat = stat(pid)?;
        Some(ProcessInfo {
            pgid: stat.pgrp,
            start: format!("linux:{}:{}", boot_id(), stat.start_ticks),
            zombie: matches!(stat.state, 'Z' | 'X'),
        })
    }

    pub(super) fn group_has_live_member(pgid: i32) -> Option<bool> {
        let entries = std::fs::read_dir("/proc").ok()?;
        Some(entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
                .and_then(stat)
                .is_some_and(|stat| stat.pgrp == pgid && !matches!(stat.state, 'Z' | 'X'))
        }))
    }

    pub(super) fn signal_group(pgid: i32, signal: i32) {
        if pgid > 0 {
            // SAFETY: plain kill(2) on a process group this owner proved.
            let _ = unsafe { libc::kill(-pgid, signal) };
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::ProcessInfo;

    pub(super) fn process_info(_pid: i32) -> Option<ProcessInfo> {
        None
    }

    pub(super) fn group_has_live_member(_pgid: i32) -> Option<bool> {
        None
    }

    pub(super) fn signal_group(_pgid: i32, _signal: i32) {}
}

/// A test-owned process group. Tests signal only groups spawned here.
#[cfg(all(test, unix))]
pub(crate) mod test_support {
    use super::{scope_process_recorder, ProcessIdentity, ProcessRecorder};
    use crate::managed_exec::{run_managed_exec, ManagedExecOutcome, ManagedExecRequest};
    use tokio_util::sync::CancellationToken;

    pub(crate) struct OwnedTestProcess {
        pub(crate) identity: ProcessIdentity,
        token: CancellationToken,
        task: Option<tokio::task::JoinHandle<ManagedExecOutcome>>,
    }

    impl OwnedTestProcess {
        /// Spawns a long `sleep` as its own group through the managed exec
        /// owner, forwarding its identity to `recorder`.
        pub(crate) async fn spawn(recorder: Option<ProcessRecorder>) -> Self {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let sender = std::sync::Mutex::new(Some(sender));
            let observer = ProcessRecorder::new(move |identity: ProcessIdentity| {
                if let Some(recorder) = recorder.as_ref() {
                    (recorder.0)(identity.clone());
                }
                if let Some(sender) = sender
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    let _ = sender.send(identity);
                }
            });
            let token = CancellationToken::new();
            let task = tokio::spawn(scope_process_recorder(
                observer,
                run_managed_exec(ManagedExecRequest {
                    argv: vec!["sleep".into(), "60".into()],
                    cwd: std::env::temp_dir(),
                    deadline_at: None,
                    cancellation_token: token.clone(),
                    max_output_bytes: 1024,
                    stdin: Vec::new(),
                    environment: None,
                    tool_name: Some("process-ownership-test".into()),
                    live_output: None,
                }),
            ));
            let identity = receiver.await.expect("test process was recorded");
            Self {
                identity,
                token,
                task: Some(task),
            }
        }

        /// Stops the group through its own managed exec owner.
        pub(crate) async fn finish(mut self) {
            self.token.cancel();
            if let Some(task) = self.task.take() {
                let _ = task.await;
            }
        }
    }

    /// Produces the host observation a generated recovery case names, using
    /// only a test-owned group recorded under `registry`.
    pub(crate) async fn process_for_generated_outcome(
        registry: &crate::hook::BackgroundExecutionRegistry,
        tool_call_id: &str,
        tool_call_doc_id: &str,
        outcome: &str,
    ) -> Option<OwnedTestProcess> {
        let recorder = registry.process_recorder(tool_call_id, tool_call_doc_id);
        match outcome {
            "stopped" => Some(OwnedTestProcess::spawn(Some(recorder)).await),
            "alreadyExited" => {
                let process = OwnedTestProcess::spawn(Some(recorder)).await;
                let _ = process.identity.stop().await;
                Some(process)
            }
            "notOwned" => None,
            other => panic!("a test-owned group cannot produce process outcome {other}"),
        }
    }

    impl Drop for OwnedTestProcess {
        fn drop(&mut self) {
            self.token.cancel();
        }
    }
}
