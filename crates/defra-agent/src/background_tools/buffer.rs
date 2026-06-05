use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::STDERR_BOUNDARY;

const LIVE_STREAM_CAPACITY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LiveToolOutputRegistry {
    inner: Arc<Mutex<HashMap<String, LiveToolOutputBuffer>>>,
}

impl LiveToolOutputRegistry {
    pub(crate) fn writer_for(&self, tool_call_id: impl Into<String>) -> LiveToolOutputWriter {
        LiveToolOutputWriter {
            registry: self.clone(),
            tool_call_id: tool_call_id.into(),
        }
    }

    pub(crate) async fn snapshot(&self, tool_call_id: &str) -> Option<LiveToolOutputSnapshot> {
        self.inner
            .lock()
            .await
            .get(tool_call_id)
            .map(LiveToolOutputBuffer::snapshot)
    }

    pub(crate) async fn remove(&self, tool_call_id: &str) {
        self.inner.lock().await.remove(tool_call_id);
    }

    async fn append(&self, tool_call_id: &str, stream: LiveOutputStream, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.inner
            .lock()
            .await
            .entry(tool_call_id.to_string())
            .or_default()
            .append(stream, bytes);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LiveToolOutputWriter {
    registry: LiveToolOutputRegistry,
    tool_call_id: String,
}

impl LiveToolOutputWriter {
    pub(crate) async fn append(&self, stream: LiveOutputStream, bytes: &[u8]) {
        self.registry
            .append(&self.tool_call_id, stream, bytes)
            .await;
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LiveToolOutputSnapshot {
    pub(crate) combined: LiveOutputStreamSnapshot,
    pub(crate) stdout_bytes: u64,
    pub(crate) stderr_bytes: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct LiveOutputStreamSnapshot {
    pub(crate) bytes: Vec<u8>,
    pub(crate) first_offset: u64,
    pub(crate) total_bytes_seen: u64,
}

#[derive(Debug, Default)]
struct LiveToolOutputBuffer {
    combined: RingBuffer,
    stdout: RingBuffer,
    stderr: RingBuffer,
    stderr_started: bool,
}

impl LiveToolOutputBuffer {
    fn append(&mut self, stream: LiveOutputStream, bytes: &[u8]) {
        match stream {
            LiveOutputStream::Stdout => {
                self.stdout.append(bytes);
                self.combined.append(bytes);
            }
            LiveOutputStream::Stderr => {
                self.stderr.append(bytes);
                if !self.stderr_started {
                    self.stderr_started = true;
                    if !self.combined.is_empty() {
                        self.combined.append(STDERR_BOUNDARY.as_bytes());
                    }
                }
                self.combined.append(bytes);
            }
        }
    }

    fn snapshot(&self) -> LiveToolOutputSnapshot {
        LiveToolOutputSnapshot {
            combined: self.combined.snapshot(),
            stdout_bytes: self.stdout.len() as u64,
            stderr_bytes: self.stderr.len() as u64,
        }
    }
}

#[derive(Debug, Default)]
struct RingBuffer {
    bytes: Vec<u8>,
    total_bytes_seen: u64,
}

impl RingBuffer {
    fn append(&mut self, bytes: &[u8]) {
        self.total_bytes_seen = self.total_bytes_seen.saturating_add(bytes.len() as u64);
        if bytes.len() >= LIVE_STREAM_CAPACITY_BYTES {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&bytes[bytes.len() - LIVE_STREAM_CAPACITY_BYTES..]);
            return;
        }

        self.bytes.extend_from_slice(bytes);
        let overflow = self.bytes.len().saturating_sub(LIVE_STREAM_CAPACITY_BYTES);
        if overflow > 0 {
            self.bytes.drain(..overflow);
        }
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn snapshot(&self) -> LiveOutputStreamSnapshot {
        LiveOutputStreamSnapshot {
            bytes: self.bytes.clone(),
            first_offset: self
                .total_bytes_seen
                .saturating_sub(self.bytes.len() as u64),
            total_bytes_seen: self.total_bytes_seen,
        }
    }
}
