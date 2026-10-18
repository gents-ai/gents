use std::time::Duration;

use tokio::process::Child;

use crate::managed_exec::output::KillReport;

const TERM_GRACE: Duration = Duration::from_millis(100);
const KILL_GRACE: Duration = Duration::from_millis(250);

pub(super) struct ManagedChild {
    pub(super) inner: Child,
    pgid: Option<i32>,
    finished: bool,
}

impl ManagedChild {
    pub(super) fn new(child: Child) -> Self {
        let pgid = child.id().and_then(|pid| i32::try_from(pid).ok());
        Self {
            inner: child,
            pgid,
            finished: false,
        }
    }

    pub(super) fn has_exited(&self) -> std::io::Result<bool> {
        if self.finished {
            return Ok(true);
        }
        let pid = self
            .pgid
            .ok_or_else(|| std::io::Error::other("managed child has no process id"))?;
        // Keep the leader waitable so its PID/PGID cannot be reused before group cleanup.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }

    pub(super) async fn terminate_service(&mut self) {
        if self.finished {
            return;
        }
        if let Some(pgid) = self.pgid {
            let _ = signal_process_group(pgid, libc::SIGTERM);
            tokio::time::sleep(TERM_GRACE).await;
            // Kill descendants before reaping the leader, even when it exited on TERM.
            let _ = signal_process_group(pgid, libc::SIGKILL);
        }
        if matches!(
            tokio::time::timeout(KILL_GRACE, self.inner.wait()).await,
            Ok(Ok(_))
        ) {
            self.finished = true;
        }
    }

    pub(super) fn pgid(&self) -> Option<i32> {
        self.pgid
    }

    pub(super) fn mark_finished(&mut self, finished: bool) {
        self.finished = finished;
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if !self.finished {
            // Last-resort cleanup cannot wait through the TERM grace period.
            if let Some(pgid) = self.pgid {
                let _ = signal_process_group(pgid, libc::SIGKILL);
            }
            let _ = self.inner.start_kill();
        }
    }
}

pub(super) async fn terminate_process_group<F>(pgid: Option<i32>, wait: &mut F) -> KillReport
where
    F: std::future::Future<Output = std::io::Result<std::process::ExitStatus>> + Unpin,
{
    let mut report = KillReport {
        pid: pgid,
        term_signal_sent: false,
        kill_signal_sent: false,
        reaped: false,
    };

    if let Some(pgid) = pgid {
        report.term_signal_sent = signal_process_group(pgid, libc::SIGTERM).is_ok();
    }
    if tokio::time::timeout(TERM_GRACE, &mut *wait).await.is_ok() {
        report.reaped = true;
        return report;
    }

    if let Some(pgid) = pgid {
        report.kill_signal_sent = signal_process_group(pgid, libc::SIGKILL).is_ok();
    }
    if tokio::time::timeout(KILL_GRACE, &mut *wait).await.is_ok() {
        report.reaped = true;
    }
    report
}

fn signal_process_group(pgid: i32, signal: i32) -> std::io::Result<()> {
    let result = unsafe { libc::kill(-pgid, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}
