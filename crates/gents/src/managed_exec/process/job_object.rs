#![cfg(windows)]

use std::io;
use std::os::windows::io::RawHandle;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::managed_exec::output::KillReport;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};

const JOB_REAP_GRACE: Duration = Duration::from_millis(250);
const TERMINATED_BY_MANAGED_EXEC: u32 = 1;
const RESUME_THREAD_FAILED: u32 = u32::MAX;

pub(super) struct ManagedChildJob {
    inner: Option<Child>,
    job: Option<OwnedHandle>,
    pid: Option<i32>,
    finished: bool,
}

#[derive(Debug)]
pub(super) struct JobSpawnError {
    pub(super) error: io::Error,
    pub(super) cleanup_confirmed: bool,
}

impl JobSpawnError {
    fn before_spawn(error: io::Error) -> Self {
        Self {
            error,
            cleanup_confirmed: true,
        }
    }
}

impl ManagedChildJob {
    pub(super) async fn spawn<F>(command: &mut Command, retain: F) -> Result<Self, JobSpawnError>
    where
        F: FnOnce(Arc<FailedJobOwner>),
    {
        let job = OwnedHandle::new_job_with_kill_on_close().map_err(JobSpawnError::before_spawn)?;
        command.creation_flags(CREATE_SUSPENDED);

        let child = command.spawn().map_err(JobSpawnError::before_spawn)?;
        let pid_u32 = child.id();
        let pid = pid_u32.and_then(|pid| i32::try_from(pid).ok());
        // The raw process HANDLE is not Send; it must not live across an await.
        let assigned = child_process_handle(&child).and_then(|process| {
            job.assign_process(process).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("assigning managed exec to Windows Job Object failed: {error}"),
                )
            })
        });
        if let Err(error) = assigned {
            return Err(cleanup_failed_spawn(error, child, job, false, pid, retain).await);
        }

        if let Some(raw_pid) = pid_u32 {
            if let Err(error) = resume_process_threads(raw_pid) {
                let error = io::Error::new(
                    error.kind(),
                    format!("resuming suspended managed exec process failed: {error}"),
                );
                return Err(cleanup_failed_spawn(error, child, job, true, pid, retain).await);
            }
        } else {
            let error = io::Error::new(
                io::ErrorKind::Other,
                "spawned managed exec child did not expose a process id",
            );
            return Err(cleanup_failed_spawn(error, child, job, true, pid, retain).await);
        }

        Ok(Self {
            inner: Some(child),
            job: Some(job),
            pid,
            finished: false,
        })
    }

    pub(super) fn pid(&self) -> Option<i32> {
        self.pid
    }

    pub(super) fn job(&self) -> JobHandle {
        JobHandle {
            raw: self.job.as_ref().expect("owned job").raw,
            pid: self.pid,
        }
    }

    pub(super) fn child_mut(&mut self) -> &mut Child {
        self.inner.as_mut().expect("owned child")
    }

    pub(super) fn into_failed_owner(mut self) -> Arc<FailedJobOwner> {
        let child = self.inner.take().expect("owned child");
        let job = self.job.take().expect("owned job");
        self.finished = true;
        Arc::new(FailedJobOwner {
            pid: self.pid,
            state: Arc::new(tokio::sync::Mutex::new(FailedJobState {
                child,
                job,
                assigned: true,
                pid: self.pid,
                confirmed: false,
            })),
        })
    }

    pub(super) fn mark_finished(&mut self, finished: bool) {
        self.finished = finished;
    }
}

pub(crate) struct FailedJobOwner {
    state: Arc<tokio::sync::Mutex<FailedJobState>>,
    pid: Option<i32>,
}

struct FailedJobState {
    child: Child,
    job: OwnedHandle,
    assigned: bool,
    pid: Option<i32>,
    confirmed: bool,
}

impl FailedJobOwner {
    pub(crate) fn pid(&self) -> Option<i32> {
        self.pid
    }

    /// Holds the exact job and child until the child is reaped and the job is empty.
    pub(crate) async fn stop_and_confirm(&self) -> bool {
        let mut state = self.state.lock().await;
        if state.confirmed {
            return true;
        }
        if state.assigned {
            let _ = state.job.terminate();
        }
        let _ = state.child.start_kill();
        let reaped = matches!(
            tokio::time::timeout(JOB_REAP_GRACE, state.child.wait()).await,
            Ok(Ok(_))
        );
        state.confirmed = reaped
            && (!state.assigned
                || job_has_no_live_processes(JobHandle {
                    raw: state.job.raw,
                    pid: state.pid,
                }));
        state.confirmed
    }
}

impl Drop for FailedJobState {
    fn drop(&mut self) {
        if !self.confirmed {
            if self.assigned {
                let _ = self.job.terminate();
            }
            let _ = self.child.start_kill();
        }
    }
}

async fn cleanup_failed_spawn<F>(
    error: io::Error,
    child: Child,
    job: OwnedHandle,
    assigned: bool,
    pid: Option<i32>,
    retain: F,
) -> JobSpawnError
where
    F: FnOnce(Arc<FailedJobOwner>),
{
    let owner = Arc::new(FailedJobOwner {
        pid,
        state: Arc::new(tokio::sync::Mutex::new(FailedJobState {
            child,
            job,
            assigned,
            pid,
            confirmed: false,
        })),
    });
    retain(owner.clone());
    let cleanup_confirmed = owner.stop_and_confirm().await;
    JobSpawnError {
        error,
        cleanup_confirmed,
    }
}

impl Drop for ManagedChildJob {
    fn drop(&mut self) {
        if !self.finished {
            if let Some(job) = &self.job {
                let _ = job.terminate();
            }
            if let Some(child) = &mut self.inner {
                let _ = child.start_kill();
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct JobHandle {
    raw: HANDLE,
    pid: Option<i32>,
}

unsafe impl Send for JobHandle {}
unsafe impl Sync for JobHandle {}

pub(super) async fn terminate_job<F>(job: JobHandle, wait: &mut F) -> KillReport
where
    F: std::future::Future<Output = io::Result<std::process::ExitStatus>> + Unpin,
{
    let mut report = KillReport {
        pid: job.pid,
        term_signal_sent: false,
        kill_signal_sent: false,
        reaped: false,
    };

    report.kill_signal_sent = terminate_job_now(job.raw).is_ok();
    if tokio::time::timeout(JOB_REAP_GRACE, &mut *wait)
        .await
        .is_ok()
    {
        report.reaped = true;
    }
    report
}

pub(super) fn job_has_no_live_processes(job: JobHandle) -> bool {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    // SAFETY: the live job handle and buffer match the requested information class.
    let success = unsafe {
        QueryInformationJobObject(
            job.raw,
            JobObjectBasicAccountingInformation,
            (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    success != 0 && info.ActiveProcesses == 0
}

struct OwnedHandle {
    raw: HANDLE,
}

unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    fn new_job_with_kill_on_close() -> io::Result<Self> {
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self { raw };

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let result = unsafe {
            SetInformationJobObject(
                job.raw,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(job)
    }

    fn assign_process(&self, process: HANDLE) -> io::Result<()> {
        let result = unsafe { AssignProcessToJobObject(self.raw, process) };
        if result == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn terminate(&self) -> io::Result<()> {
        terminate_job_now(self.raw)
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.raw);
        }
    }
}

fn terminate_job_now(job: HANDLE) -> io::Result<()> {
    let result = unsafe { TerminateJobObject(job, TERMINATED_BY_MANAGED_EXEC) };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn child_process_handle(child: &Child) -> io::Result<HANDLE> {
    child
        .raw_handle()
        .map(raw_handle_to_handle)
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "spawned managed exec child exited"))
}

fn raw_handle_to_handle(raw: RawHandle) -> HANDLE {
    raw as HANDLE
}

fn resume_process_threads(pid: u32) -> io::Result<()> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = OwnedHandle { raw: snapshot };

    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    let mut has_entry = unsafe { Thread32First(snapshot.raw, &mut entry) } != 0;
    let mut resumed = false;

    while has_entry {
        if entry.th32OwnerProcessID == pid {
            resume_thread(entry.th32ThreadID)?;
            resumed = true;
        }
        has_entry = unsafe { Thread32Next(snapshot.raw, &mut entry) } != 0;
    }

    if resumed {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no suspended threads found for managed exec process {pid}"),
        ))
    }
}

fn resume_thread(thread_id: u32) -> io::Result<()> {
    let raw = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let thread = OwnedHandle { raw };

    let result = unsafe { ResumeThread(thread.raw) };
    if result == RESUME_THREAD_FAILED {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_wait_owner_retains_exact_running_job_until_stop() {
        let mut command = Command::new("cmd.exe");
        command.args(["/C", "ping -n 60 127.0.0.1 > nul"]);
        let child = ManagedChildJob::spawn(&mut command, |_| panic!("setup unexpectedly failed"))
            .await
            .ok()
            .expect("managed job");
        let pid = child.pid();
        let owner = child.into_failed_owner();
        assert_eq!(owner.pid(), pid);
        assert!(owner.stop_and_confirm().await);
        assert!(owner.stop_and_confirm().await);
    }

    #[tokio::test]
    async fn failed_setup_confirms_suspended_child_and_job_exit() {
        for assigned in [false, true] {
            let job = OwnedHandle::new_job_with_kill_on_close().unwrap();
            let mut command = Command::new("cmd.exe");
            command
                .args(["/C", "exit", "0"])
                .creation_flags(CREATE_SUSPENDED)
                .kill_on_drop(true);
            let child = command.spawn().unwrap();
            let pid = child.id().and_then(|pid| i32::try_from(pid).ok());
            if assigned {
                job.assign_process(child_process_handle(&child).unwrap())
                    .unwrap();
            }
            let retained = std::sync::Mutex::new(None);
            let result = cleanup_failed_spawn(
                io::Error::other("setup refused"),
                child,
                job,
                assigned,
                pid,
                |owner| *retained.lock().unwrap() = Some(owner),
            )
            .await;
            assert!(
                result.cleanup_confirmed,
                "suspended child must be reaped and assigned job empty"
            );
            assert_eq!(result.error.to_string(), "setup refused");
            let owner = retained.into_inner().unwrap().unwrap();
            assert!(owner.stop_and_confirm().await);
            let mut state = owner.state.lock().await;
            assert!(state.child.try_wait().unwrap().is_some());
            if assigned {
                assert!(job_has_no_live_processes(JobHandle {
                    raw: state.job.raw,
                    pid
                }));
            }
        }
    }

    #[tokio::test]
    async fn cancelled_setup_cleanup_keeps_owner_for_later_stop() {
        let job = OwnedHandle::new_job_with_kill_on_close().unwrap();
        let mut command = Command::new("cmd.exe");
        command
            .args(["/C", "exit", "0"])
            .creation_flags(CREATE_SUSPENDED)
            .kill_on_drop(true);
        let child = command.spawn().unwrap();
        let pid = child.id().and_then(|pid| i32::try_from(pid).ok());
        job.assign_process(child_process_handle(&child).unwrap())
            .unwrap();
        let retained = Arc::new(std::sync::Mutex::new(None));
        let held = Arc::new(std::sync::Mutex::new(None));
        let (published, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn({
            let retained = retained.clone();
            let held = held.clone();
            async move {
                cleanup_failed_spawn(
                    io::Error::other("setup refused"),
                    child,
                    job,
                    true,
                    pid,
                    |owner| {
                        *held.lock().unwrap() = Some(owner.state.clone().try_lock_owned().unwrap());
                        *retained.lock().unwrap() = Some(owner);
                        published.send(()).unwrap();
                    },
                )
                .await
            }
        });
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(held.lock().unwrap().take());
        let owner = retained.lock().unwrap().take().unwrap();
        assert!(owner.stop_and_confirm().await);
    }

    #[tokio::test]
    async fn service_registry_keeps_failed_job_until_stop_is_confirmed() {
        let job = OwnedHandle::new_job_with_kill_on_close().unwrap();
        let mut command = Command::new("cmd.exe");
        command
            .args(["/C", "exit", "0"])
            .creation_flags(CREATE_SUSPENDED)
            .kill_on_drop(true);
        let child = command.spawn().unwrap();
        let pid = child.id().and_then(|pid| i32::try_from(pid).ok());
        job.assign_process(child_process_handle(&child).unwrap())
            .unwrap();
        let owner = Arc::new(FailedJobOwner {
            pid,
            state: Arc::new(tokio::sync::Mutex::new(FailedJobState {
                child,
                job,
                assigned: true,
                pid,
                confirmed: false,
            })),
        });
        let id = super::super::registry::retain_failed_service(
            owner.clone(),
            "cmd.exe".into(),
            Some("test-service".into()),
            super::super::registry::ManagedExecKind::PersistentService,
        );
        let guard = owner.state.clone().lock_owned().await;
        let task = tokio::spawn(super::super::registry::stop_failed_service(id));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(super::super::registry::active_executor_snapshots()
            .iter()
            .any(|entry| entry.id == id && Some(entry.pid) == pid));
        drop(guard);
        assert!(super::super::registry::stop_failed_service(id).await);
        assert!(!super::super::registry::active_executor_snapshots()
            .iter()
            .any(|entry| entry.id == id));
    }
}
