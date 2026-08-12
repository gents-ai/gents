use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::background_tools::LiveToolOutputWriter;

use super::output::ManagedExecOutcome;

mod capture;
#[cfg(windows)]
mod job_object;
#[cfg(unix)]
mod process_group;
mod registry;

use capture::{join_capture_with_timeout, spawn_optional_capped};
#[cfg(windows)]
use job_object::{terminate_job, ManagedChildJob};
#[cfg(unix)]
use process_group::{terminate_process_group, ManagedChild};
use registry::ActiveExecGuard;

pub(crate) use registry::{active_executor_snapshots, ManagedExecKind};

const CAPTURE_DRAIN_AFTER_CHILD_EXIT: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(crate) struct ManagedExecRequest {
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) deadline_at: Option<DateTime<Utc>>,
    pub(crate) cancellation_token: CancellationToken,
    pub(crate) max_output_bytes: usize,
    pub(crate) stdin: Vec<u8>,
    /// `None` inherits the daemon environment. `Some` replaces it wholesale
    /// with the supplied, already-filtered environment.
    pub(crate) environment: Option<HashMap<String, String>>,
    pub(crate) tool_name: Option<String>,
    pub(crate) live_output: Option<LiveToolOutputWriter>,
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
    if let Some(environment) = request.environment.as_ref() {
        command.env_clear().envs(environment);
    }

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
    let _active = pid.map(|pid| {
        ActiveExecGuard::insert(
            pid,
            program.clone(),
            request.tool_name.clone(),
            ManagedExecKind::ForegroundCommand,
        )
    });

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
    let stdout_task = spawn_optional_capped(
        stdout,
        max_output_bytes,
        request
            .live_output
            .clone()
            .map(|writer| (writer, crate::background_tools::LiveOutputStream::Stdout)),
    );
    let stderr_task = spawn_optional_capped(
        stderr,
        max_output_bytes,
        request
            .live_output
            .clone()
            .map(|writer| (writer, crate::background_tools::LiveOutputStream::Stderr)),
    );

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
                    let (stdout, stderr) = tokio::join!(
                        join_capture_with_timeout(stdout_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
                        join_capture_with_timeout(stderr_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
                    );
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
                    let _ = tokio::join!(
                        join_capture_with_timeout(stdout_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
                        join_capture_with_timeout(stderr_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
                    );
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
    let (stdout, stderr) = tokio::join!(
        join_capture_with_timeout(stdout_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
        join_capture_with_timeout(stderr_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
    );

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
    if let Some(environment) = request.environment.as_ref() {
        command.env_clear().envs(environment);
    }

    let mut child = match ManagedChildJob::spawn(&mut command) {
        Ok(child) => child,
        Err(error) => {
            return ManagedExecOutcome::SpawnFailed {
                error: error.to_string(),
            };
        }
    };

    let pid = child.pid();
    let _active = pid.map(|pid| {
        ActiveExecGuard::insert(
            pid,
            program.clone(),
            request.tool_name.clone(),
            ManagedExecKind::ForegroundCommand,
        )
    });

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
    let stdout_task = spawn_optional_capped(
        stdout,
        max_output_bytes,
        request
            .live_output
            .clone()
            .map(|writer| (writer, crate::background_tools::LiveOutputStream::Stdout)),
    );
    let stderr_task = spawn_optional_capped(
        stderr,
        max_output_bytes,
        request
            .live_output
            .clone()
            .map(|writer| (writer, crate::background_tools::LiveOutputStream::Stderr)),
    );

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
                    let (stdout, stderr) = tokio::join!(
                        join_capture_with_timeout(stdout_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
                        join_capture_with_timeout(stderr_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
                    );
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
                    let _ = tokio::join!(
                        join_capture_with_timeout(stdout_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
                        join_capture_with_timeout(stderr_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
                    );
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
    let (stdout, stderr) = tokio::join!(
        join_capture_with_timeout(stdout_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
        join_capture_with_timeout(stderr_task, CAPTURE_DRAIN_AFTER_CHILD_EXIT),
    );

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

#[derive(Debug)]
pub(crate) struct SpawnManagedProcessRequest {
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) environment: Option<HashMap<String, String>>,
    pub(crate) tool_name: Option<String>,
    pub(crate) kind: ManagedExecKind,
}

pub(crate) struct ManagedProcess {
    #[cfg(unix)]
    child: ManagedChild,
    #[cfg(windows)]
    child: ManagedChildJob,
    pub(crate) stdin: Option<tokio::process::ChildStdin>,
    pub(crate) stdout: Option<tokio::process::ChildStdout>,
    pub(crate) stderr: Option<tokio::process::ChildStderr>,
    _guard: Option<ActiveExecGuard>,
}

impl ManagedProcess {
    pub(crate) async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.inner.wait().await
    }

    pub(crate) async fn terminate(&mut self) {
        #[cfg(unix)]
        {
            let pgid = self.child.pgid();
            {
                let mut wait = Box::pin(self.child.inner.wait());
                let _ = terminate_process_group(pgid, &mut wait).await;
            }
            self.child.mark_finished(true);
        }
        #[cfg(windows)]
        {
            let job = self.child.job();
            let mut wait = Box::pin(self.child.inner.wait());
            let _ = terminate_job(job, &mut wait).await;
            self.child.mark_finished(true);
        }
    }
}

#[cfg(unix)]
pub(crate) async fn spawn_managed_process(
    request: SpawnManagedProcessRequest,
) -> Result<ManagedProcess, String> {
    let Some((program, args)) = request.argv.split_first() else {
        return Err("managed exec argv must not be empty".to_string());
    };

    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(environment) = request.environment.as_ref() {
        command.env_clear().envs(environment);
    }
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = command
        .spawn()
        .map(ManagedChild::new)
        .map_err(|error| error.to_string())?;
    let pid = child.pgid();
    let guard = pid.map(|pid| {
        ActiveExecGuard::insert(
            pid,
            program.clone(),
            request.tool_name.clone(),
            request.kind,
        )
    });
    Ok(ManagedProcess {
        stdin: child.inner.stdin.take(),
        stdout: child.inner.stdout.take(),
        stderr: child.inner.stderr.take(),
        child,
        _guard: guard,
    })
}

#[cfg(windows)]
pub(crate) async fn spawn_managed_process(
    request: SpawnManagedProcessRequest,
) -> Result<ManagedProcess, String> {
    let Some((program, args)) = request.argv.split_first() else {
        return Err("managed exec argv must not be empty".to_string());
    };

    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(environment) = request.environment.as_ref() {
        command.env_clear().envs(environment);
    }
    let mut child = ManagedChildJob::spawn(&mut command).map_err(|error| error.to_string())?;
    let pid = child.pid();
    let guard = pid.map(|pid| {
        ActiveExecGuard::insert(
            pid,
            program.clone(),
            request.tool_name.clone(),
            request.kind,
        )
    });
    Ok(ManagedProcess {
        stdin: child.inner.stdin.take(),
        stdout: child.inner.stdout.take(),
        stderr: child.inner.stderr.take(),
        child,
        _guard: guard,
    })
}

#[cfg(all(not(unix), not(windows)))]
pub(crate) async fn spawn_managed_process(
    _request: SpawnManagedProcessRequest,
) -> Result<ManagedProcess, String> {
    Err("ManagedExec process-tree termination is not yet supported on this platform".to_string())
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
