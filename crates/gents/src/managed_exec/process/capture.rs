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

#[cfg(unix)]
struct NativePipe<R>(R);

#[cfg(unix)]
impl<R> AsyncRead for NativePipe<R>
where
    R: AsyncRead + Unpin + std::os::fd::AsRawFd,
{
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let permit = std::task::ready!(tokio::task::coop::poll_proceed(cx));
        let available = buf.initialize_unfilled();
        // Tokio child pipes are nonblocking. Probe the owned descriptor because
        // reactor readiness can lag an already-exited child under scheduler load.
        let read = unsafe {
            libc::read(
                self.0.as_raw_fd(),
                available.as_mut_ptr().cast(),
                available.len(),
            )
        };
        if read >= 0 {
            permit.made_progress();
            buf.advance(read as usize);
            return std::task::Poll::Ready(Ok(()));
        }
        let error = std::io::Error::last_os_error();
        if !matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ) {
            return std::task::Poll::Ready(Err(error));
        }
        let result = std::pin::Pin::new(&mut self.0).poll_read(cx, buf);
        if result.is_ready() {
            permit.made_progress();
        }
        result
    }
}

/// The reader must be a Tokio child pipe, whose descriptor is nonblocking.
#[cfg(unix)]
pub(super) fn spawn_native_capped<R>(
    reader: Option<R>,
    max_bytes: usize,
    live_output: Option<(LiveToolOutputWriter, LiveOutputStream)>,
) -> CaptureTask
where
    R: AsyncRead + Unpin + Send + std::os::fd::AsRawFd + 'static,
{
    spawn_optional_capped(reader.map(NativePipe), max_bytes, live_output)
}

/// Tokio's cooperative budget can yield even when a pipe has data or EOF ready.
/// Post-exit probes bypass that budget; buffered output remains bounded by bytes.
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
            match tokio::task::unconstrained(read_retry_interrupted(&mut reader, &mut buf))
                .now_or_never()
            {
                Some(result) => result,
                None => {
                    let remaining = POST_EXIT_DRAIN_READ_BUDGET.saturating_sub(post_exit_read_time);
                    if remaining.is_zero() {
                        lock_output(&output).truncated = true;
                        break;
                    }
                    match tokio::time::timeout(
                        remaining,
                        read_retry_interrupted(&mut reader, &mut buf),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => {
                            post_exit_read_time = POST_EXIT_DRAIN_READ_BUDGET;
                            match tokio::task::unconstrained(read_retry_interrupted(
                                &mut reader,
                                &mut buf,
                            ))
                            .now_or_never()
                            {
                                Some(result) => result,
                                None => {
                                    lock_output(&output).truncated = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        } else {
            tokio::select! {
                biased;
                _ = exit_notify.notified() => continue,
                result = read_retry_interrupted(&mut reader, &mut buf) => result,
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

/// Interrupted reads transfer no bytes and do not establish an I/O failure.
/// Yield between retries so cancellation and the existing drain deadline remain
/// observable even when a reader is repeatedly interrupted.
async fn read_retry_interrupted<R>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize>
where
    R: AsyncRead + Unpin,
{
    loop {
        match reader.read(buf).await {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                tokio::task::yield_now().await;
            }
            result => return result,
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
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    struct ReadSteps(VecDeque<Result<&'static [u8], std::io::ErrorKind>>);

    impl AsyncRead for ReadSteps {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            match self.0.pop_front().unwrap_or(Ok(b"")) {
                Ok(bytes) => {
                    buf.put_slice(bytes);
                    Poll::Ready(Ok(()))
                }
                Err(kind) => Poll::Ready(Err(kind.into())),
            }
        }
    }

    #[tokio::test]
    async fn interrupted_pipe_reads_preserve_output_and_eof() {
        for exited in [false, true] {
            let output = Arc::new(Mutex::new(OutputCapture {
                bytes: Vec::new(),
                truncated: false,
            }));
            read_capped(
                ReadSteps(VecDeque::from([
                    Err(std::io::ErrorKind::Interrupted),
                    Ok(b"captured".as_slice()),
                    Err(std::io::ErrorKind::Interrupted),
                    Ok(b"".as_slice()),
                ])),
                1024,
                None,
                Arc::clone(&output),
                Arc::new(std::sync::atomic::AtomicBool::new(exited)),
                Arc::new(Notify::new()),
            )
            .await;
            let output = lock_output(&output);
            assert_eq!(output.bytes, b"captured", "child exited: {exited}");
            assert!(!output.truncated, "child exited: {exited}");
        }
    }

    struct InterruptedForever(Arc<std::sync::atomic::AtomicUsize>);

    impl AsyncRead for InterruptedForever {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Poll::Ready(Err(std::io::ErrorKind::Interrupted.into()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_interruptions_do_not_extend_the_post_exit_deadline() {
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let task = spawn_optional_capped(Some(InterruptedForever(Arc::clone(&polls))), 1024, None);
        let output = tokio::spawn(join_capture_after_child_exit(task));
        while polls.load(std::sync::atomic::Ordering::Relaxed) < 2 {
            assert!(
                !output.is_finished(),
                "capture stopped before retrying the interrupted read"
            );
            tokio::task::yield_now().await;
        }
        tokio::time::advance(POST_EXIT_DRAIN_READ_BUDGET).await;
        let output = output.await.unwrap();
        assert!(output.bytes.is_empty());
        assert!(output.truncated);
    }

    #[tokio::test]
    async fn an_io_failure_after_output_keeps_the_capture_incomplete() {
        let task = spawn_optional_capped(
            Some(ReadSteps(VecDeque::from([
                Ok(b"captured".as_slice()),
                Err(std::io::ErrorKind::BrokenPipe),
            ]))),
            1024,
            None,
        );
        let output = join_capture_after_child_exit(task).await;
        assert_eq!(output.bytes, b"captured");
        assert!(output.truncated);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_eof_is_visible_before_the_reactor_observes_exit() {
        let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let reader = tokio::net::UnixStream::from_std(reader).unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reader = NativePipe(reader);
        let mut bytes = [0; 1];
        let read = reader.read(&mut bytes).now_or_never();
        assert!(matches!(read, Some(Ok(0))), "{read:?}");
    }

    struct DelayedReadyRead(bool);

    impl AsyncRead for DelayedReadyRead {
        #[expect(
            clippy::disallowed_methods,
            reason = "simulates a read that overruns the drain budget within one poll"
        )]
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

    struct CooperativeEof(bool);

    impl AsyncRead for CooperativeEof {
        #[expect(
            clippy::disallowed_methods,
            reason = "simulates a read that overruns the drain budget within one poll"
        )]
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let permit = std::task::ready!(tokio::task::coop::poll_proceed(cx));
            permit.made_progress();
            if !self.0 {
                self.0 = true;
                for _ in 0..128 {
                    if let Poll::Ready(permit) = tokio::task::coop::poll_proceed(cx) {
                        permit.made_progress();
                    }
                }
                std::thread::sleep(POST_EXIT_DRAIN_READ_BUDGET * 2);
                buf.put_slice(b"x");
            }
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn ready_eof_with_exhausted_cooperative_budget_is_complete() {
        let task = spawn_optional_capped(Some(CooperativeEof(false)), 1024, None);
        let output = join_capture_after_child_exit(task).await;
        assert_eq!(output.bytes, b"x");
        assert!(!output.truncated);
    }

    #[tokio::test(start_paused = true)]
    async fn a_descendant_held_pipe_without_eof_is_truncated() {
        let (reader, _writer) = tokio::io::duplex(64);
        let task = spawn_optional_capped(Some(reader), 1024, None);
        let output = join_capture_after_child_exit(task).await;
        assert!(output.bytes.is_empty());
        assert!(output.truncated);
    }

    #[tokio::test]
    async fn a_continuously_ready_pipe_still_has_a_bounded_drain() {
        let task = spawn_optional_capped(Some(tokio::io::repeat(b'x')), 1024, None);
        let output = join_capture_after_child_exit(task).await;
        assert_eq!(output.bytes, vec![b'x'; 1024]);
        assert!(output.truncated);
    }

    struct EofAfterReadiness {
        polls: Arc<std::sync::atomic::AtomicUsize>,
        ready_at: tokio::time::Instant,
        ready: bool,
        data: Option<&'static [u8]>,
    }

    impl AsyncRead for EofAfterReadiness {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
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
                if let Some(data) = self.data.take() {
                    buf.put_slice(data);
                }
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_capture_accepts_eof_ready_after_its_drain_deadline_expires() {
        let output = delayed_capture(None).await;
        assert!(output.bytes.is_empty());
        assert!(!output.truncated);
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_capture_drains_data_and_eof_ready_after_its_deadline() {
        let output = delayed_capture(Some(b"tree-hash\n")).await;
        assert_eq!(output.bytes, b"tree-hash\n");
        assert!(!output.truncated);
    }

    async fn delayed_capture(data: Option<&'static [u8]>) -> OutputCapture {
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
                data,
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
        output
    }
}
