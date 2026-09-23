use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Notify;

use crate::background_tools::{LiveOutputStream, LiveToolOutputWriter};

// After the direct child exits, the finite OS pipe can still contain its
// output. Drain that buffer, but do not let an orphan descendant that inherited
// the pipe write forever. This is not a cap on pre-exit command output, and
// time spent committing canonical output does not consume the read budget.
const POST_EXIT_DRAIN_READ_BUDGET: Duration = Duration::from_millis(100);
const POST_EXIT_DRAIN_BYTE_BUDGET: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(super) struct OutputCapture {
    pub(super) bytes: Vec<u8>,
    pub(super) truncated: bool,
}

pub(super) struct CaptureTask {
    handle: tokio::task::JoinHandle<()>,
    output: Arc<Mutex<OutputCapture>>,
    child_exited: Arc<std::sync::atomic::AtomicBool>,
    exit_notify: Arc<Notify>,
}

pub(super) fn spawn_optional_capped<R>(
    reader: Option<R>,
    max_bytes: usize,
    live_output: Option<(LiveToolOutputWriter, LiveOutputStream)>,
) -> CaptureTask
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let output = Arc::new(Mutex::new(OutputCapture {
        bytes: Vec::new(),
        truncated: false,
    }));
    let task_output = Arc::clone(&output);
    let child_exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let exit_notify = Arc::new(Notify::new());
    let task_exited = Arc::clone(&child_exited);
    let task_notify = Arc::clone(&exit_notify);
    let handle = tokio::spawn(async move {
        let Some(reader) = reader else {
            return;
        };
        read_capped(
            reader,
            max_bytes,
            live_output,
            task_output,
            task_exited,
            task_notify,
        )
        .await;
    });
    CaptureTask {
        handle,
        output,
        child_exited,
        exit_notify,
    }
}

async fn read_capped<R>(
    mut reader: R,
    max_bytes: usize,
    live_output: Option<(LiveToolOutputWriter, LiveOutputStream)>,
    output: Arc<Mutex<OutputCapture>>,
    child_exited: Arc<std::sync::atomic::AtomicBool>,
    exit_notify: Arc<Notify>,
) where
    R: AsyncRead + Unpin,
{
    let mut buf = [0u8; 8192];
    let mut post_exit_read_time = Duration::ZERO;
    let mut post_exit_bytes = 0usize;
    loop {
        let exited_before_read = child_exited.load(std::sync::atomic::Ordering::Acquire);
        let read_started = Instant::now();
        let next = if exited_before_read {
            let remaining = POST_EXIT_DRAIN_READ_BUDGET.saturating_sub(post_exit_read_time);
            if remaining.is_zero() {
                lock_output(&output).truncated = true;
                break;
            }
            match tokio::time::timeout(remaining, reader.read(&mut buf)).await {
                Ok(result) => result,
                Err(_) => {
                    lock_output(&output).truncated = true;
                    break;
                }
            }
        } else {
            tokio::select! {
                biased;
                _ = exit_notify.notified() => continue,
                result = reader.read(&mut buf) => result,
            }
        };
        let read = match next {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => {
                lock_output(&output).truncated = true;
                break;
            }
        };
        if exited_before_read {
            post_exit_read_time = post_exit_read_time.saturating_add(read_started.elapsed());
            post_exit_bytes = post_exit_bytes.saturating_add(read);
        }
        {
            let mut output = lock_output(&output);
            let remaining = max_bytes.saturating_sub(output.bytes.len());
            if remaining == 0 {
                output.truncated = true;
            } else {
                let take = remaining.min(read);
                output.bytes.extend_from_slice(&buf[..take]);
                if take < read {
                    output.truncated = true;
                }
            }
        }
        if let Some((writer, stream)) = &live_output {
            writer.append(*stream, &buf[..read]).await;
        }
        if post_exit_bytes >= POST_EXIT_DRAIN_BYTE_BUDGET
            || post_exit_read_time >= POST_EXIT_DRAIN_READ_BUDGET
        {
            lock_output(&output).truncated = true;
            break;
        }
    }
}

fn lock_output(output: &Mutex<OutputCapture>) -> std::sync::MutexGuard<'_, OutputCapture> {
    output
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) async fn join_capture_after_child_exit(task: CaptureTask) -> OutputCapture {
    task.child_exited
        .store(true, std::sync::atomic::Ordering::Release);
    task.exit_notify.notify_one();
    let result = task.handle.await;
    let mut output = lock_output(&task.output).clone();
    if result.is_err() {
        output.truncated = true;
    }
    output
}
