use std::time::Duration;

use tokio::process::Child;

use crate::tools::exec::output::KillReport;

const TERM_GRACE: Duration = Duration::from_millis(100);
const KILL_GRACE: Duration = Duration::from_millis(250);

/// Spawn into a fresh process group using native spawn attributes. A pre-exec
/// callback forces fork, where concurrent children can retain unrelated pipe
/// writers until exec and prevent a completed command from observing EOF.
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

pub(super) async fn terminate_process_group(child: &mut ManagedChild) -> KillReport {
    let pgid = child.pgid();
    let mut report = KillReport {
        pid: pgid,
        term_signal_sent: false,
        kill_signal_sent: false,
        reaped: false,
    };

    if let Some(pgid) = pgid {
        report.term_signal_sent = signal_process_group(pgid, libc::SIGTERM).is_ok();
        // Keep the leader waitable until the whole group has received KILL.
        // Reaping it first permits its process-group ID to be reused.
        tokio::time::sleep(TERM_GRACE).await;
        report.kill_signal_sent = signal_process_group(pgid, libc::SIGKILL).is_ok();
    }
    if matches!(
        tokio::time::timeout(KILL_GRACE, child.inner.wait()).await,
        Ok(Ok(_))
    ) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[tokio::test]
    async fn leader_term_exit_does_not_leave_term_ignoring_child() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "trap 'exit 0' TERM; (trap '' TERM; printf 'ready\\n'; while :; do sleep 60; done) & wait"])
            .process_group(0)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = ManagedChild::new(command.spawn().unwrap());
        let identity =
            crate::tools::exec::ownership::ProcessIdentity::of_live(child.pgid().unwrap()).unwrap();
        let stdout = child.inner.stdout.take().unwrap();
        let mut line = String::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            BufReader::new(stdout).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(line, "ready\n");
        let report = terminate_process_group(&mut child).await;
        child.mark_finished(report.reaped);
        assert!(report.term_signal_sent && report.kill_signal_sent && report.reaped);
        assert_eq!(
            identity
                .await_signalled_group_exit(crate::tools::exec::ownership::KILL_GRACE)
                .await,
            crate::tools::exec::ownership::ProcessObservation::Exited
        );
    }
}
