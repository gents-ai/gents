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
        // The pre_exec setsid() call makes the child's pid its process-group id.
        // If setsid() fails, Command::spawn returns an error before this point.
        let pgid = child.id().and_then(|pid| i32::try_from(pid).ok());
        Self {
            inner: child,
            pgid,
            finished: false,
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
            // Drop is last-resort cleanup, so skip TERM grace and force the group down.
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
