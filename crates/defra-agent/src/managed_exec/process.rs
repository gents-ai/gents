use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

use super::output::{KillReport, ManagedExecOutcome};

const TERM_GRACE: Duration = Duration::from_millis(100);
const KILL_GRACE: Duration = Duration::from_millis(250);

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

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ActiveManagedExecSnapshot {
    pub(crate) id: u64,
    pub(crate) pid: i32,
    pub(crate) argv0: String,
    pub(crate) tool_name: Option<String>,
    pub(crate) started_at: String,
    pub(crate) age_ms: i64,
}

#[derive(Debug, Clone)]
struct ActiveManagedExecRecord {
    id: u64,
    pid: i32,
    argv0: String,
    tool_name: Option<String>,
    started_at: DateTime<Utc>,
    started_instant: Instant,
}

static NEXT_EXEC_ID: AtomicU64 = AtomicU64::new(1);
static ACTIVE_EXECS: OnceLock<Mutex<BTreeMap<u64, ActiveManagedExecRecord>>> = OnceLock::new();

fn active_execs() -> &'static Mutex<BTreeMap<u64, ActiveManagedExecRecord>> {
    ACTIVE_EXECS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(crate) fn active_executor_snapshots() -> Vec<ActiveManagedExecSnapshot> {
    active_execs()
        .lock()
        .expect("managed exec active registry mutex poisoned")
        .values()
        .map(|record| ActiveManagedExecSnapshot {
            id: record.id,
            pid: record.pid,
            argv0: record.argv0.clone(),
            tool_name: record.tool_name.clone(),
            started_at: record.started_at.to_rfc3339(),
            age_ms: i64::try_from(record.started_instant.elapsed().as_millis()).unwrap_or(i64::MAX),
        })
        .collect()
}

struct ActiveExecGuard {
    id: u64,
}

impl ActiveExecGuard {
    fn insert(pid: i32, argv0: String, tool_name: Option<String>) -> Self {
        let id = NEXT_EXEC_ID.fetch_add(1, Ordering::Relaxed);
        let record = ActiveManagedExecRecord {
            id,
            pid,
            argv0,
            tool_name,
            started_at: Utc::now(),
            started_instant: Instant::now(),
        };
        active_execs()
            .lock()
            .expect("managed exec active registry mutex poisoned")
            .insert(id, record);
        Self { id }
    }
}

impl Drop for ActiveExecGuard {
    fn drop(&mut self) {
        active_execs()
            .lock()
            .expect("managed exec active registry mutex poisoned")
            .remove(&self.id);
    }
}

#[derive(Debug)]
struct OutputCapture {
    bytes: Vec<u8>,
    truncated: bool,
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
                    child.finished = true;
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
                    child.finished = true;
                    return ManagedExecOutcome::SpawnFailed {
                        error: format!("waiting for managed exec failed: {error}"),
                    };
                }
            }
        }
    };

    let kill = terminate_process_group(child_pgid, &mut wait).await;
    drop(wait);
    child.finished = kill.reaped;
    let stdout = join_capture(stdout_task).await;
    let stderr = join_capture(stderr_task).await;

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

#[cfg(not(unix))]
pub(crate) async fn run_managed_exec(_request: ManagedExecRequest) -> ManagedExecOutcome {
    unimplemented!("Windows ManagedExec process termination: see #236")
}

#[derive(Debug, Clone, Copy)]
enum OutcomeKind {
    TimedOut,
    Cancelled,
}

#[cfg(unix)]
struct ManagedChild {
    inner: Child,
    pgid: Option<i32>,
    finished: bool,
}

#[cfg(unix)]
impl ManagedChild {
    fn new(child: Child) -> Self {
        let pgid = child.id().and_then(|pid| i32::try_from(pid).ok());
        Self {
            inner: child,
            pgid,
            finished: false,
        }
    }

    fn pgid(&self) -> Option<i32> {
        self.pgid
    }
}

#[cfg(unix)]
impl Drop for ManagedChild {
    fn drop(&mut self) {
        if !self.finished {
            if let Some(pgid) = self.pgid {
                let _ = signal_process_group(pgid, libc::SIGKILL);
            }
            let _ = self.inner.start_kill();
        }
    }
}

async fn read_optional_capped<R>(reader: Option<R>, max_bytes: usize) -> OutputCapture
where
    R: AsyncRead + Unpin,
{
    let Some(reader) = reader else {
        return OutputCapture {
            bytes: Vec::new(),
            truncated: false,
        };
    };
    read_capped(reader, max_bytes).await
}

async fn read_capped<R>(mut reader: R, max_bytes: usize) -> OutputCapture
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 8192];
    loop {
        let read = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        let remaining = max_bytes.saturating_sub(bytes.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let take = remaining.min(read);
        bytes.extend_from_slice(&buf[..take]);
        if take < read {
            truncated = true;
        }
    }
    OutputCapture { bytes, truncated }
}

async fn join_capture(task: tokio::task::JoinHandle<OutputCapture>) -> OutputCapture {
    task.await.unwrap_or(OutputCapture {
        bytes: Vec::new(),
        truncated: true,
    })
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

#[cfg(unix)]
async fn terminate_process_group<F>(pgid: Option<i32>, wait: &mut F) -> KillReport
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

#[cfg(unix)]
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
