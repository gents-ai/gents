#![cfg(windows)]

use std::io;
use std::os::windows::io::RawHandle;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::managed_exec::output::KillReport;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};

const JOB_REAP_GRACE: Duration = Duration::from_millis(250);
const TERMINATED_BY_MANAGED_EXEC: u32 = 1;
const RESUME_THREAD_FAILED: u32 = u32::MAX;

pub(super) struct ManagedChildJob {
    pub(super) inner: Child,
    job: OwnedHandle,
    pid: Option<i32>,
    finished: bool,
}

impl ManagedChildJob {
    pub(super) fn spawn(command: &mut Command) -> io::Result<Self> {
        let job = OwnedHandle::new_job_with_kill_on_close()?;
        command.creation_flags(CREATE_SUSPENDED);

        let mut child = command.spawn()?;
        let pid_u32 = child.id();
        let pid = pid_u32.and_then(|pid| i32::try_from(pid).ok());
        let process = child_process_handle(&child)?;

        if let Err(error) = job.assign_process(process) {
            let _ = child.start_kill();
            return Err(io::Error::new(
                error.kind(),
                format!("assigning managed exec to Windows Job Object failed: {error}"),
            ));
        }

        if let Some(pid) = pid_u32 {
            if let Err(error) = resume_process_threads(pid) {
                let _ = job.terminate();
                let _ = child.start_kill();
                return Err(io::Error::new(
                    error.kind(),
                    format!("resuming suspended managed exec process failed: {error}"),
                ));
            }
        } else {
            let _ = job.terminate();
            let _ = child.start_kill();
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "spawned managed exec child did not expose a process id",
            ));
        }

        Ok(Self {
            inner: child,
            job,
            pid,
            finished: false,
        })
    }

    pub(super) fn pid(&self) -> Option<i32> {
        self.pid
    }

    pub(super) fn job(&self) -> JobHandle {
        JobHandle {
            raw: self.job.raw,
            pid: self.pid,
        }
    }

    pub(super) fn mark_finished(&mut self, finished: bool) {
        self.finished = finished;
    }
}

impl Drop for ManagedChildJob {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.job.terminate();
            let _ = self.inner.start_kill();
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
