use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::output::ManagedExecOutcome;

mod capture;
#[cfg(windows)]
mod job_object;
#[cfg(unix)]
mod process_group;
mod registry;

use capture::{join_capture, join_capture_with_timeout, read_optional_capped};
#[cfg(windows)]
use job_object::{terminate_job, ManagedChildJob};
#[cfg(unix)]
use process_group::{terminate_process_group, ManagedChild};
use registry::ActiveExecGuard;

pub(crate) use registry::active_executor_snapshots;

const CAPTURE_DRAIN_AFTER_FAILED_REAP: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub(crate) struct ManagedExecRequest {
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) deadline_at: Option<DateTime<Utc>>,
    pub(crate) cancellation_token: CancellationToken,
    pub(crate) max_output_bytes: usize,
    pub(crate) stdin: Vec<u8>,
    pub(crate) tool_name: Option<String>,
}

#[cfg(unix)]
pub(crate) async fn run_managed_exec(request: ManagedExecRequest) -> ManagedExecOutcome {
    let Some((program, args)) = request.argv.split_first() else {
        return ManagedExecOutcome::SpawnFailed {
            error: "managed exec argv must not be empty".to_string(),
        };
    };

    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&request.cwd)
        .stdin(if request.stdin.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = match command.spawn() {
        Ok(child) => ManagedChild::new(child),
        Err(error) => {
            return ManagedExecOutcome::SpawnFailed {
                error: error.to_string(),
            };
        }
    };

    let pid = child.pgid();
    let _active =
        pid.map(|pid| ActiveExecGuard::insert(pid, program.clone(), request.tool_name.clone()));

    if !request.stdin.is_empty() {
        if let Some(mut stdin) = child.inner.stdin.take() {
            let input = request.stdin;
            tokio::spawn(async move {
                let _ = stdin.write_all(&input).await;
            });
        }
    }

    let stdout = child.inner.stdout.take();
    let stderr = child.inner.stderr.take();
    let max_output_bytes = request.max_output_bytes;
    let stdout_task = tokio::spawn(read_optional_capped(stdout, max_output_bytes));
    let stderr_task = tokio::spawn(read_optional_capped(stderr, max_output_bytes));

    let child_pgid = child.pgid();
    let mut wait = Box::pin(child.inner.wait());
    let deadline = sleep_until_deadline(request.deadline_at);
    tokio::pin!(deadline);

    let outcome_kind = tokio::select! {
        biased;
        _ = request.cancellation_token.cancelled() => OutcomeKind::Cancelled,
        _ = &mut deadline => OutcomeKind::TimedOut,
        status = &mut wait => {
            match status {
                Ok(status) => {
                    drop(wait);
                    child.mark_finished(true);
                    let stdout = join_capture(stdout_task).await;
                    let stderr = join_capture(stderr_task).await;
                    return ManagedExecOutcome::Exited {
                        code: status.code(),
                        stdout: stdout.bytes,
                        stderr: stderr.bytes,
                        stdout_truncated: stdout.truncated,
                        stderr_truncated: stderr.truncated,
                    };
                }
                Err(error) => {
                    drop(wait);
                    child.mark_finished(true);
                    return ManagedExecOutcome::SpawnFailed {
                        error: format!("waiting for managed exec failed: {error}"),
                    };
                }
            }
        }
    };

    let kill = terminate_process_group(child_pgid, &mut wait).await;
    drop(wait);
    child.mark_finished(kill.reaped);
    let capture_timeout = (!kill.reaped).then_some(CAPTURE_DRAIN_AFTER_FAILED_REAP);
    let stdout = join_capture_with_timeout(stdout_task, capture_timeout).await;
    let stderr = join_capture_with_timeout(stderr_task, capture_timeout).await;

    match outcome_kind {
        OutcomeKind::TimedOut => ManagedExecOutcome::TimedOut {
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            kill,
        },
        OutcomeKind::Cancelled => ManagedExecOutcome::Cancelled {
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            kill,
        },
    }
}

#[cfg(windows)]
pub(crate) async fn run_managed_exec(request: ManagedExecRequest) -> ManagedExecOutcome {
    let Some((program, args)) = request.argv.split_first() else {
        return ManagedExecOutcome::SpawnFailed {
            error: "managed exec argv must not be empty".to_string(),
        };
    };

    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&request.cwd)
        .stdin(if request.stdin.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match ManagedChildJob::spawn(&mut command) {
        Ok(child) => child,
        Err(error) => {
            return ManagedExecOutcome::SpawnFailed {
                error: error.to_string(),
            };
        }
    };

    let pid = child.pid();
    let _active =
        pid.map(|pid| ActiveExecGuard::insert(pid, program.clone(), request.tool_name.clone()));

    if !request.stdin.is_empty() {
        if let Some(mut stdin) = child.inner.stdin.take() {
            let input = request.stdin;
            tokio::spawn(async move {
                let _ = stdin.write_all(&input).await;
            });
        }
    }

    let stdout = child.inner.stdout.take();
    let stderr = child.inner.stderr.take();
    let max_output_bytes = request.max_output_bytes;
    let stdout_task = tokio::spawn(read_optional_capped(stdout, max_output_bytes));
    let stderr_task = tokio::spawn(read_optional_capped(stderr, max_output_bytes));

    let job = child.job();
    let mut wait = Box::pin(child.inner.wait());
    let deadline = sleep_until_deadline(request.deadline_at);
    tokio::pin!(deadline);

    let outcome_kind = tokio::select! {
        biased;
        _ = request.cancellation_token.cancelled() => OutcomeKind::Cancelled,
        _ = &mut deadline => OutcomeKind::TimedOut,
        status = &mut wait => {
            match status {
                Ok(status) => {
                    drop(wait);
                    child.mark_finished(true);
                    let stdout = join_capture(stdout_task).await;
                    let stderr = join_capture(stderr_task).await;
                    return ManagedExecOutcome::Exited {
                        code: status.code(),
                        stdout: stdout.bytes,
                        stderr: stderr.bytes,
                        stdout_truncated: stdout.truncated,
                        stderr_truncated: stderr.truncated,
                    };
                }
                Err(error) => {
                    drop(wait);
                    child.mark_finished(true);
                    return ManagedExecOutcome::SpawnFailed {
                        error: format!("waiting for managed exec failed: {error}"),
                    };
                }
            }
        }
    };

    let kill = terminate_job(job, &mut wait).await;
    drop(wait);
    child.mark_finished(kill.reaped);
    let capture_timeout = (!kill.reaped).then_some(CAPTURE_DRAIN_AFTER_FAILED_REAP);
    let stdout = join_capture_with_timeout(stdout_task, capture_timeout).await;
    let stderr = join_capture_with_timeout(stderr_task, capture_timeout).await;

    match outcome_kind {
        OutcomeKind::TimedOut => ManagedExecOutcome::TimedOut {
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            kill,
        },
        OutcomeKind::Cancelled => ManagedExecOutcome::Cancelled {
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            kill,
        },
    }
}

#[cfg(all(not(unix), not(windows)))]
pub(crate) async fn run_managed_exec(_request: ManagedExecRequest) -> ManagedExecOutcome {
    ManagedExecOutcome::SpawnFailed {
        error: "ManagedExec process-tree termination is not yet supported on this platform"
            .to_string(),
    }
}

#[derive(Debug, Clone, Copy)]
enum OutcomeKind {
    TimedOut,
    Cancelled,
}

async fn sleep_until_deadline(deadline_at: Option<DateTime<Utc>>) {
    let Some(deadline_at) = deadline_at else {
        std::future::pending::<()>().await;
        return;
    };
    let now = Utc::now();
    if now >= deadline_at {
        return;
    }
    let duration = (deadline_at - now).to_std().unwrap_or(Duration::ZERO);
    tokio::time::sleep(duration).await;
}
