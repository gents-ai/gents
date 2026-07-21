use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::background_tools::{LiveOutputStream, LiveToolOutputWriter};

#[derive(Debug)]
pub(super) struct OutputCapture {
    pub(super) bytes: Vec<u8>,
    pub(super) truncated: bool,
}

pub(super) async fn read_optional_capped<R>(
    reader: Option<R>,
    max_bytes: usize,
    live_output: Option<(LiveToolOutputWriter, LiveOutputStream)>,
) -> OutputCapture
where
    R: AsyncRead + Unpin,
{
    let Some(reader) = reader else {
        return OutputCapture {
            bytes: Vec::new(),
            truncated: false,
        };
    };
    read_capped(reader, max_bytes, live_output).await
}

async fn read_capped<R>(
    mut reader: R,
    max_bytes: usize,
    live_output: Option<(LiveToolOutputWriter, LiveOutputStream)>,
) -> OutputCapture
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
        if let Some((writer, stream)) = &live_output {
            writer.append(*stream, &buf[..read]).await;
        }
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

pub(super) async fn join_capture(task: tokio::task::JoinHandle<OutputCapture>) -> OutputCapture {
    task.await.unwrap_or(OutputCapture {
        bytes: Vec::new(),
        truncated: true,
    })
}

pub(super) async fn join_capture_with_timeout(
    mut task: tokio::task::JoinHandle<OutputCapture>,
    timeout: Option<Duration>,
) -> OutputCapture {
    let Some(timeout) = timeout else {
        return join_capture(task).await;
    };

    tokio::select! {
        result = &mut task => result.unwrap_or(OutputCapture {
            bytes: Vec::new(),
            truncated: true,
        }),
        _ = tokio::time::sleep(timeout) => {
            task.abort();
            OutputCapture {
                bytes: Vec::new(),
                truncated: true,
            }
        }
    }
}
