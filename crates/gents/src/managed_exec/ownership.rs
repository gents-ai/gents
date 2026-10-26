use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ManagedExecError {
    #[error("cannot inspect process ownership records: {error}")]
    InspectRecords { error: std::io::Error },
    #[error("cannot enumerate process ownership records: {error}")]
    EnumerateRecords { error: std::io::Error },
    #[error("cannot read process ownership record {}: {error}", path.display())]
    ReadRecord {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("cannot decode process ownership record {}: {error}", path.display())]
    DecodeRecord {
        path: PathBuf,
        error: serde_json::Error,
    },
}

// Process-group ids are reused; termination requires the recorded leader's OS start identity.
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

    pub(crate) const fn may_terminate(self) -> bool {
        matches!(self, Self::Running)
    }

    pub(crate) const fn retain_process_record(self) -> bool {
        !matches!(self, Self::Exited)
    }
}

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

    pub const fn cancel_reply(self) -> CancelProcessReply {
        match self {
            Self::Stopped => CancelProcessReply::Cancelled,
            Self::AlreadyExited | Self::NotOwned => CancelProcessReply::Lost,
            Self::StillRunning => CancelProcessReply::Unverified,
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: i32,
    pub(crate) pgid: i32,
    pub(crate) start: String,
}

impl ProcessIdentity {
    #[cfg(unix)]
    pub(crate) fn of_live(pid: i32) -> Option<Self> {
        let info = platform::process_info(pid)?;
        (!info.zombie).then_some(Self {
            pid,
            pgid: info.pgid,
            start: info.start,
        })
    }

    pub(crate) fn observe(&self) -> ProcessObservation {
        if self.pid <= 0 || self.pgid <= 0 {
            return ProcessObservation::Unowned;
        }
        match platform::process_info(self.pid) {
            Some(info) if !info.zombie && info.start == self.start && info.pgid == self.pgid => {
                ProcessObservation::Running
            }
            _ => match platform::group_has_live_member(self.pgid) {
                Some(false) => ProcessObservation::Exited,
                Some(true) | None => ProcessObservation::Unowned,
            },
        }
    }

    fn observe_signalled_group(&self) -> ProcessObservation {
        match platform::group_has_live_member(self.pgid) {
            Some(false) => ProcessObservation::Exited,
            Some(true) | None => ProcessObservation::Running,
        }
    }

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

    /// Nothing is signalled unless `before` admits termination.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProcessRecord {
    pub(crate) tool_call_id: String,
    pub(crate) tool_call_doc_id: String,
    pub(crate) identity: ProcessIdentity,
}

pub(crate) trait HostRecord:
    Serialize + serde::de::DeserializeOwned + Clone + Send + 'static
{
    fn key(&self) -> &str;
}

impl HostRecord for ProcessRecord {
    fn key(&self) -> &str {
        &self.tool_call_id
    }
}

#[derive(Debug, Clone)]
pub(crate) enum HostRecordStore<R> {
    Volatile(Arc<std::sync::Mutex<rapidhash::RapidHashMap<String, R>>>),
    Durable(PathBuf),
}

pub(crate) type ProcessRecordStore = HostRecordStore<ProcessRecord>;

impl<R> Default for HostRecordStore<R> {
    fn default() -> Self {
        Self::Volatile(Arc::default())
    }
}

fn record_path(dir: &std::path::Path, key: &str) -> PathBuf {
    let name = key
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    dir.join(format!("{name}.json"))
}

impl<R> HostRecordStore<R>
where
    R: HostRecord,
{
    fn volatile(
        records: &std::sync::Mutex<rapidhash::RapidHashMap<String, R>>,
    ) -> std::sync::MutexGuard<'_, rapidhash::RapidHashMap<String, R>> {
        records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // Sync the record and its directory entry before claiming durable ownership.
    pub(crate) fn write(&self, record: &R) -> std::io::Result<()> {
        let dir = match self {
            Self::Volatile(records) => {
                Self::volatile(records).insert(record.key().to_owned(), record.clone());
                return Ok(());
            }
            Self::Durable(dir) => dir,
        };
        let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
        std::fs::create_dir_all(dir)?;
        let path = record_path(dir, record.key());
        let temporary = path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&temporary, &path)?;
        std::fs::File::open(dir).and_then(|dir| dir.sync_all())
    }

    pub(crate) fn read(&self, key: &str) -> Option<R> {
        let dir = match self {
            Self::Volatile(records) => return Self::volatile(records).get(key).cloned(),
            Self::Durable(dir) => dir,
        };
        let bytes = std::fs::read(record_path(dir, key)).ok()?;
        match serde_json::from_slice::<R>(&bytes) {
            Ok(record) if record.key() == key => Some(record),
            Ok(_) | Err(_) => {
                tracing::warn!(key, "ignoring unreadable host record");
                None
            }
        }
    }

    pub(crate) fn remove(&self, key: &str) {
        let dir = match self {
            Self::Volatile(records) => {
                Self::volatile(records).remove(key);
                return;
            }
            Self::Durable(dir) => dir,
        };
        match std::fs::remove_file(record_path(dir, key)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(key, %error, "failed to remove host record");
            }
        }
    }

    pub(crate) fn list(&self) -> Vec<R> {
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
            .filter_map(|bytes| serde_json::from_slice::<R>(&bytes).ok())
            .collect()
    }

    pub(crate) fn try_list(&self) -> Result<Vec<R>, ManagedExecError> {
        let dir = match self {
            Self::Volatile(records) => {
                return Ok(Self::volatile(records).values().cloned().collect())
            }
            Self::Durable(dir) => dir,
        };
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(ManagedExecError::InspectRecords { error }),
        };
        let mut records = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| ManagedExecError::EnumerateRecords { error })?
                .path();
            if !path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                continue;
            }
            let bytes = std::fs::read(&path).map_err(|error| ManagedExecError::ReadRecord {
                path: path.clone(),
                error,
            })?;
            records.push(serde_json::from_slice::<R>(&bytes).map_err(|error| {
                ManagedExecError::DecodeRecord {
                    path: path.clone(),
                    error,
                }
            })?);
        }
        Ok(records)
    }
}

type RecordFn = dyn Fn(ProcessIdentity) + Send + Sync;

#[derive(Clone)]
pub(crate) struct ProcessRecorder {
    _record: Arc<RecordFn>,
}

impl ProcessRecorder {
    pub(crate) fn new(record: impl Fn(ProcessIdentity) + Send + Sync + 'static) -> Self {
        Self {
            _record: Arc::new(record),
        }
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

#[cfg(unix)]
pub(super) fn record_spawned(pid: i32) {
    let Ok(recorder) = PROCESS_RECORDER.try_with(Clone::clone) else {
        return;
    };
    match ProcessIdentity::of_live(pid) {
        Some(identity) => (recorder._record)(identity),
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
        let rest = raw.get(raw.rfind(')')? + 2..)?;
        let fields = rest.split_whitespace().collect::<Vec<_>>();
        Some(Stat {
            state: fields.first()?.chars().next()?,
            pgrp: fields.get(2)?.parse().ok()?,
            start_ticks: fields.get(19)?.parse().ok()?,
        })
    }

    /// Start ticks restart at each boot, so a start identity without the boot
    /// id cannot distinguish a process from a later boot's pid reuse.
    fn boot_id() -> Option<String> {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
    }

    pub(super) fn process_info(pid: i32) -> Option<ProcessInfo> {
        process_info_with_boot_id(pid, boot_id())
    }

    pub(super) fn process_info_with_boot_id(
        pid: i32,
        boot_id: Option<String>,
    ) -> Option<ProcessInfo> {
        if pid <= 0 {
            return None;
        }
        let boot_id = boot_id?;
        let stat = stat(pid)?;
        Some(ProcessInfo {
            pgid: stat.pgrp,
            start: format!("linux:{boot_id}:{}", stat.start_ticks),
            zombie: matches!(stat.state, 'Z' | 'X'),
        })
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn unreadable_boot_id_proves_no_start_identity() {
            let pid = std::process::id() as i32;
            assert!(super::process_info_with_boot_id(pid, None).is_none());
            assert!(super::process_info_with_boot_id(pid, Some("boot".into())).is_some());
        }
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

// A test-owned process group. Tests signal only groups spawned here.
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
        pub(crate) async fn spawn(recorder: Option<ProcessRecorder>) -> Self {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let sender = std::sync::Mutex::new(Some(sender));
            let observer = ProcessRecorder::new(move |identity: ProcessIdentity| {
                if let Some(recorder) = recorder.as_ref() {
                    (recorder._record)(identity.clone());
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

        pub(crate) async fn finish(mut self) {
            self.token.cancel();
            if let Some(task) = self.task.take() {
                let _ = task.await;
            }
        }
    }

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
