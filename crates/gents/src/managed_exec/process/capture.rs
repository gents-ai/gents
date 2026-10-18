use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::FutureExt;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Notify;

use crate::background_tools::{LiveOutputStream, LiveToolOutputWriter};

// After the direct child exits, drain the OS pipe but do not let an orphan
// descendant that inherited it write forever.
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
            match reader.read(&mut buf).now_or_never() {
                Some(result) => result,
                None => {
                    let remaining = POST_EXIT_DRAIN_READ_BUDGET.saturating_sub(post_exit_read_time);
                    if remaining.is_zero() {
                        lock_output(&output).truncated = true;
                        break;
                    }
                    match tokio::time::timeout(remaining, reader.read(&mut buf)).await {
                        Ok(result) => result,
                        Err(_) => {
                            // A delayed capture task can first be polled after
                            // the timer has expired, even when the pipe is now
                            // at EOF. Check once without waiting before treating
                            // the drain as incomplete.
                            match tokio::task::unconstrained(reader.read(&mut buf)).now_or_never() {
                                Some(Ok(0)) => break,
                                Some(Ok(read)) => {
                                    {
                                        let mut output = lock_output(&output);
                                        let remaining =
                                            max_bytes.saturating_sub(output.bytes.len());
                                        let take = remaining.min(read);
                                        output.bytes.extend_from_slice(&buf[..take]);
                                        output.truncated = true;
                                    }
                                    if let Some((writer, stream)) = &live_output {
                                        writer.append(*stream, &buf[..read]).await;
                                    }
                                }
                                Some(Err(_)) | None => {
                                    lock_output(&output).truncated = true;
                                }
                            }
                            break;
                        }
                    }
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
        if post_exit_bytes >= POST_EXIT_DRAIN_BYTE_BUDGET {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    struct DelayedReadyRead(bool);

    impl AsyncRead for DelayedReadyRead {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if !self.0 {
                self.0 = true;
                std::thread::sleep(POST_EXIT_DRAIN_READ_BUDGET * 2);
                buf.put_slice(b"x");
            }
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn ready_eof_after_a_scheduling_stall_is_complete() {
        let task = spawn_optional_capped(Some(DelayedReadyRead(false)), 65536, None);
        let output = join_capture_after_child_exit(task).await;
        assert_eq!(output.bytes, b"x");
        assert!(!output.truncated);
    }

    struct EofAfterReadiness {
        polls: Arc<std::sync::atomic::AtomicUsize>,
        ready_at: tokio::time::Instant,
        ready: bool,
    }

    impl AsyncRead for EofAfterReadiness {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            self.polls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Readiness arrives while the expiration poll checks the pipe:
            // that poll must return Pending, then wake the task so the next
            // nonblocking read observes EOF.
            if !self.ready && tokio::time::Instant::now() >= self.ready_at {
                self.ready = true;
                while let Poll::Ready(permit) = tokio::task::coop::poll_proceed(cx) {
                    // Model a reader that consumed the task's cooperative
                    // budget while processing ready I/O before it can yield.
                    permit.made_progress();
                }
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            if self.ready {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_capture_accepts_eof_ready_after_its_drain_deadline_expires() {
        let keepalive = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ready_at =
            tokio::time::Instant::now() + POST_EXIT_DRAIN_READ_BUDGET + Duration::from_millis(1);
        let task = spawn_optional_capped(
            Some(EofAfterReadiness {
                polls: Arc::clone(&polls),
                ready_at,
                ready: false,
            }),
            1024,
            None,
        );
        task.child_exited
            .store(true, std::sync::atomic::Ordering::Release);
        task.exit_notify.notify_one();

        while polls.load(std::sync::atomic::Ordering::Relaxed) < 2 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(POST_EXIT_DRAIN_READ_BUDGET + Duration::from_millis(1)).await;

        let output = join_capture_after_child_exit(task).await;
        keepalive.abort();
        assert!(output.bytes.is_empty());
        assert!(!output.truncated);
    }
}
