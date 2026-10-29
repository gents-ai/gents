use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "ownership/records_tests.rs"]
mod record_store_tests;

// Process-group ids are reused; termination requires the recorded leader's OS start identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessObservation {
    Running,
    Exited,
    Unowned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveProcessEvidence {
    NeverSpawned,
    SpawnedUnverified,
    Exited,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum NativeTerminalIntent {
    Completed,
    Cancelled,
    TimedOut,
    Failed,
    Panicked,
}

pub(crate) const fn may_settle_native(
    intent: NativeTerminalIntent,
    evidence: LiveProcessEvidence,
) -> bool {
    matches!(intent, NativeTerminalIntent::Completed)
        || !matches!(evidence, LiveProcessEvidence::SpawnedUnverified)
}

pub(crate) fn native_terminal_intent(
    outcome: Option<&crate::tool_call_lifecycle::ToolOutcome>,
) -> NativeTerminalIntent {
    use crate::tool_call_lifecycle::ToolOutcome;
    match outcome {
        Some(ToolOutcome::Completed(_)) => NativeTerminalIntent::Completed,
        Some(ToolOutcome::Cancelled) => NativeTerminalIntent::Cancelled,
        Some(ToolOutcome::TimedOut { .. }) => NativeTerminalIntent::TimedOut,
        Some(ToolOutcome::Failed { .. }) => NativeTerminalIntent::Failed,
        None => NativeTerminalIntent::Panicked,
    }
}

pub(crate) fn native_terminal_needs_confirmation(
    outcome: Option<&crate::tool_call_lifecycle::ToolOutcome>,
) -> bool {
    !may_settle_native(
        native_terminal_intent(outcome),
        LiveProcessEvidence::SpawnedUnverified,
    )
}

pub(crate) const fn retire_stop_receipt(terminal_committed: bool) -> bool {
    terminal_committed
}

pub(crate) const fn settle_live_cancel(
    released: bool,
    evidence: LiveProcessEvidence,
) -> ProcessStopOutcome {
    if released
        && matches!(
            evidence,
            LiveProcessEvidence::NeverSpawned | LiveProcessEvidence::Exited
        )
    {
        ProcessStopOutcome::Stopped
    } else {
        ProcessStopOutcome::StillRunning
    }
}

#[derive(Default)]
pub(crate) struct LiveProcessEvidenceOwner(std::sync::atomic::AtomicU8);

impl LiveProcessEvidenceOwner {
    pub(crate) fn initialized(&self) {
        self.0.store(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn spawned(&self) {
        self.0.store(2, std::sync::atomic::Ordering::SeqCst);
    }
    pub(crate) fn exited(&self) {
        self.0.store(3, std::sync::atomic::Ordering::SeqCst);
    }
    pub(crate) fn mark_unverified_failure(&self) {
        let _ = self.0.compare_exchange(
            2,
            4,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        );
    }
    pub(crate) fn get(&self) -> LiveProcessEvidence {
        match self.0.load(std::sync::atomic::Ordering::SeqCst) {
            1 => LiveProcessEvidence::NeverSpawned,
            3 => LiveProcessEvidence::Exited,
            _ => LiveProcessEvidence::SpawnedUnverified,
        }
    }
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
    #[cfg(all(unix, test))]
    pub(crate) fn of_live(pid: i32) -> Option<Self> {
        let info = platform::process_info(pid)?;
        (!info.zombie).then_some(Self {
            pid,
            pgid: info.pgid,
            start: info.start,
        })
    }

    #[cfg(unix)]
    fn of_waitable_child(pid: i32) -> Option<Self> {
        let info = platform::process_info(pid)?;
        Some(Self {
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

mod records;
#[cfg(test)]
use records::record_path;
pub(crate) use records::{HostRecord, HostRecordStore};

impl HostRecord for ProcessRecord {
    fn key(&self) -> &str {
        &self.tool_call_doc_id
    }

    fn matches_key(&self, key: &str) -> bool {
        self.tool_call_doc_id == key || self.tool_call_id == key
    }
}

pub(crate) type ProcessRecordStore = HostRecordStore<ProcessRecord>;

type RecordFn = dyn Fn(ProcessIdentity) + Send + Sync;

#[derive(Clone)]
pub(crate) struct ProcessRecorder {
    _record: Arc<RecordFn>,
    evidence: Option<Arc<LiveProcessEvidenceOwner>>,
    failure_decided: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(windows)]
    retain_failed_job: Option<Arc<dyn Fn(Arc<super::process::FailedJobOwner>) + Send + Sync>>,
    #[cfg(windows)]
    release_failed_job: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl ProcessRecorder {
    pub(crate) fn new(record: impl Fn(ProcessIdentity) + Send + Sync + 'static) -> Self {
        Self {
            _record: Arc::new(record),
            evidence: None,
            failure_decided: None,
            #[cfg(windows)]
            retain_failed_job: None,
            #[cfg(windows)]
            release_failed_job: None,
        }
    }

    #[cfg(windows)]
    pub(crate) fn with_failed_job_owner(
        mut self,
        retain: impl Fn(Arc<super::process::FailedJobOwner>) + Send + Sync + 'static,
        release: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.retain_failed_job = Some(Arc::new(retain));
        self.release_failed_job = Some(Arc::new(release));
        self
    }

    #[cfg(windows)]
    pub(super) fn retain_failed_job(&self, owner: Arc<super::process::FailedJobOwner>) -> bool {
        if let Some(retain) = &self.retain_failed_job {
            retain(owner);
            true
        } else {
            false
        }
    }

    #[cfg(windows)]
    pub(super) fn release_failed_job(&self) {
        if let Some(release) = &self.release_failed_job {
            release();
        }
    }

    pub(crate) fn with_failure_decision(
        mut self,
        report: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.failure_decided = Some(Arc::new(report));
        self
    }

    pub(crate) fn with_evidence(mut self, evidence: Arc<LiveProcessEvidenceOwner>) -> Self {
        evidence.initialized();
        self.evidence = Some(evidence);
        self
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

pub(super) fn current_process_recorder() -> Option<ProcessRecorder> {
    PROCESS_RECORDER.try_with(Clone::clone).ok()
}

pub(crate) fn record_failure_decision(recorder: Option<&ProcessRecorder>) {
    if let Some(report) = recorder.and_then(|recorder| recorder.failure_decided.as_ref()) {
        report();
    }
}

pub(super) fn record_spawn_attempt(recorder: Option<&ProcessRecorder>) {
    if let Some(evidence) = recorder.and_then(|recorder| recorder.evidence.as_ref()) {
        evidence.spawned();
    }
}

pub(super) fn record_unconfirmed_spawn_failure(recorder: Option<&ProcessRecorder>) {
    if let Some(evidence) = recorder.and_then(|recorder| recorder.evidence.as_ref()) {
        evidence.mark_unverified_failure();
    }
}

pub(super) fn record_confirmed_exit(recorder: Option<&ProcessRecorder>) {
    if let Some(evidence) = recorder.and_then(|recorder| recorder.evidence.as_ref()) {
        evidence.exited();
    }
}

#[cfg(unix)]
pub(super) fn record_spawned(
    recorder: Option<&ProcessRecorder>,
    pid: i32,
) -> Option<ProcessIdentity> {
    let recorder = recorder?;
    match ProcessIdentity::of_waitable_child(pid) {
        Some(identity) => {
            (recorder._record)(identity.clone());
            Some(identity)
        }
        None => {
            tracing::warn!(
                pid,
                "spawned background process exited before it was recorded"
            );
            None
        }
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

    unsafe extern "C" {
        fn gents_darwin_process_info(
            pid: libc::c_int,
            pgid: *mut libc::c_int,
            seconds: *mut u64,
            microseconds: *mut u32,
            zombie: *mut libc::c_int,
        ) -> libc::c_int;
    }

    fn process_info_checked(pid: i32) -> Result<Option<ProcessInfo>, ()> {
        if pid <= 0 {
            return Ok(None);
        }
        let (mut pgid, mut seconds, mut microseconds, mut zombie) = (0, 0, 0, 0);
        // SAFETY: the shim writes only to these valid output pointers.
        let result = unsafe {
            gents_darwin_process_info(pid, &mut pgid, &mut seconds, &mut microseconds, &mut zombie)
        };
        match result {
            1 => Ok(Some(ProcessInfo {
                pgid,
                start: format!("darwin:{seconds}.{microseconds:06}"),
                zombie: zombie != 0,
            })),
            0 => Ok(None),
            _ => Err(()),
        }
    }

    pub(super) fn process_info(pid: i32) -> Option<ProcessInfo> {
        process_info_checked(pid).ok().flatten()
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
            for &pid in &pids[..count] {
                match process_info_checked(pid) {
                    Ok(Some(info)) if !info.zombie && info.pgid as u32 == pgid => {
                        return Some(true);
                    }
                    Err(()) => return None,
                    _ => {}
                }
            }
            return Some(false);
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

    #[cfg(test)]
    mod tests {
        #[test]
        fn unreadable_boot_id_proves_no_start_identity() {
            let pid = std::process::id() as i32;
            assert!(super::process_info_with_boot_id(pid, None).is_none());
            assert!(super::process_info_with_boot_id(pid, Some("boot".into())).is_some());
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

    #[tokio::test]
    async fn exited_unreaped_child_retains_exact_start_identity() {
        let mut child = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("read ignored")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id().unwrap()).unwrap();
        let live = ProcessIdentity::of_waitable_child(pid).expect("live child identity");
        drop(child.stdin.take());
        tokio::task::spawn_blocking(move || {
            // WNOWAIT leaves the child waitable, preventing PID reuse before identity capture.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            assert_eq!(result, 0);
        })
        .await
        .unwrap();
        let identity = ProcessIdentity::of_waitable_child(pid).expect("unreaped child identity");
        assert_eq!(identity, live);
        child.wait().await.unwrap();
    }

    #[tokio::test]
    async fn call_keyed_process_record_remains_cancellable_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let registry = crate::hook::BackgroundExecutionRegistry::default()
            .with_process_records(directory.path().into());
        let process =
            OwnedTestProcess::spawn(Some(registry.process_recorder("old-call", "physical-tool")))
                .await;
        std::fs::rename(
            super::record_path(directory.path(), "physical-tool"),
            super::record_path(directory.path(), "old-call"),
        )
        .unwrap();
        let restarted = crate::hook::BackgroundExecutionRegistry::default()
            .with_process_records(directory.path().into());
        assert!(restarted.process_record("old-call", "wrong-tool").is_none());
        assert_eq!(
            restarted.stop_execution("old-call", "physical-tool").await,
            super::ProcessStopOutcome::Stopped
        );
        restarted.forget_process_record("old-call", "physical-tool");
        assert!(restarted.process_record_list().is_empty());
        process.finish().await;
    }

    impl Drop for OwnedTestProcess {
        fn drop(&mut self) {
            self.token.cancel();
        }
    }
}
