use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::background_tools::{LiveOutputStream, LiveToolOutputWriter};

#[derive(Clone, Debug)]
pub(super) struct OutputCapture {
    pub(super) bytes: Vec<u8>,
    pub(super) truncated: bool,
}

pub(super) struct CaptureTask {
    handle: tokio::task::JoinHandle<()>,
    output: Arc<Mutex<OutputCapture>>,
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
    let handle = tokio::spawn(async move {
        let Some(reader) = reader else {
            return;
        };
        read_capped(reader, max_bytes, live_output, task_output).await;
    });
    CaptureTask { handle, output }
}

async fn read_capped<R>(
    mut reader: R,
    max_bytes: usize,
    live_output: Option<(LiveToolOutputWriter, LiveOutputStream)>,
    output: Arc<Mutex<OutputCapture>>,
) where
    R: AsyncRead + Unpin,
{
    let mut buf = [0u8; 8192];
    loop {
        let read = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => {
                lock_output(&output).truncated = true;
                break;
            }
        };
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
    }
}

fn lock_output(output: &Mutex<OutputCapture>) -> std::sync::MutexGuard<'_, OutputCapture> {
    output
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) async fn join_capture_with_timeout(
    mut task: CaptureTask,
    timeout: Duration,
) -> OutputCapture {
    tokio::select! {
        result = &mut task.handle => {
            let mut output = lock_output(&task.output).clone();
            if result.is_err() {
                output.truncated = true;
            }
            output
        },
        _ = tokio::time::sleep(timeout) => {
            task.handle.abort();
            let _ = task.handle.await;
            let mut output = lock_output(&task.output).clone();
            output.truncated = true;
            output
        }
    }
}
