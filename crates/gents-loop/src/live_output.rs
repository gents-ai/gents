use rapidhash::RapidHashMap;
use std::any::{Any, TypeId};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gents_protocol::output::PayloadPresentation;
use tokio::sync::Mutex;

mod presentation;
mod writer;

pub use presentation::{CapturedChannel, CommandPresentationLayout};
pub use writer::LiveToolOutputWriter;

pub const COMMAND_OUTPUT_META_PREFIX: &str = "gents_exec: ";

#[derive(Debug, thiserror::Error)]
pub enum LiveOutputError {
    #[error("canonical output registration disappeared before receipt")]
    ReceiptRegistrationMissing,
    #[error("canonical output receipt underflow")]
    ReceiptUnderflow,
    #[error("canonical output registration disappeared before presentation")]
    PresentationRegistrationMissing,
    #[error("prepared tool presentation does not match the terminal native text")]
    PreparedPresentationMismatch,
    #[error("command capture receipts do not cover the complete raw stdout/stderr")]
    CaptureReceiptsIncomplete,
    #[error("command truncation returned an invalid UTF-8 range")]
    InvalidTruncationRange,
    #[error("command truncation did not preserve its declared raw prefix")]
    TruncationPrefixMismatch,
    #[error("command truncation did not preserve its declared raw suffix")]
    TruncationSuffixMismatch,
    #[error("command metadata did not serialize as a JSON object")]
    MetadataNotObject,
    #[error("command receipt range exceeds canonical chunk")]
    ReceiptRangeExceedsChunk,
    #[error("command presentation range has a receipt gap")]
    ReceiptGap,
    #[error("plugin receipt has no registered physical tool invocation")]
    PluginReceiptUnregistered,
    #[error("plugin execution receipt conflicts with this physical tool invocation")]
    PluginReceiptConflict,
    #[error("deferred writes have no registered physical tool invocation")]
    DeferredWritesUnregistered,
    #[error("command presentation requires a canonical live-output writer")]
    CanonicalWriterRequired,
    #[error(transparent)]
    Integer(#[from] std::num::TryFromIntError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("canonical output append failed")]
    Appender {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

type Result<T, E = LiveOutputError> = std::result::Result<T, E>;

pub const COMMAND_OUTPUT_VOLATILE_FIELDS: [&str; 2] = ["duration_ms", "hint"];

/// Rendered between stdout and stderr when a live buffer's combined stream
/// starts carrying stderr bytes; shared verbatim with `gents::tools::background::status`.
pub const STDERR_BOUNDARY: &str = "\n--- stderr ---\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Default)]
pub struct LiveToolOutputRegistry {
    inner: Arc<Mutex<RapidHashMap<String, LiveToolOutputState>>>,
}

#[derive(Debug, Default)]
struct LiveToolOutputState {
    receipts: [Vec<OutputReceipt>; 2],
    prepared: Option<PreparedToolPresentation>,
    plugin_receipt: Option<gents_protocol::plugin::PluginExecutionReceipt>,
    deferred_writes: Option<DeferredToolWrites>,
}

/// Host data a tool returned beside its output, to be committed by the
/// terminal of the call that returned it. The loop only carries it from
/// dispatch to terminalization; the host that produced it reads it back.
/// Distinct host producers must not replace each other's contributions. Each
/// producer retains ownership of the batch stored under its concrete type.
#[derive(Debug, Clone)]
pub struct DeferredToolWrites(RapidHashMap<TypeId, Arc<dyn Any + Send + Sync>>);

impl DeferredToolWrites {
    pub fn new<T>(writes: T) -> Self
    where
        T: Any + Send + Sync,
    {
        let writes: Arc<dyn Any + Send + Sync> = Arc::new(writes);
        Self(RapidHashMap::from_iter([(TypeId::of::<T>(), writes)]))
    }

    pub fn downcast_ref<T>(&self) -> Option<&T>
    where
        T: Any,
    {
        self.0.get(&TypeId::of::<T>())?.downcast_ref()
    }
}

#[derive(Debug, Clone)]
struct OutputReceipt {
    channel_start: u64,
    channel_end: u64,
    source_start: u64,
    source_end: u64,
}

#[derive(Debug, Clone)]
pub struct PreparedToolPresentation {
    pub expected_text: String,
    pub presentation: PayloadPresentation,
}

pub trait CanonicalOutputAppender: std::fmt::Debug + Send + Sync {
    fn append<'a>(
        &'a self,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<std::ops::Range<u64>, LiveOutputError>> + Send + 'a>>;
}

impl LiveToolOutputRegistry {
    pub async fn canonical_writer_for(
        &self,
        tool_call_doc_id: String,
        appender: Arc<dyn CanonicalOutputAppender>,
    ) -> LiveToolOutputWriter {
        let tool_call_id = tool_call_doc_id;
        self.inner
            .lock()
            .await
            .entry(tool_call_id.clone())
            .or_default();
        LiveToolOutputWriter {
            registry: self.clone(),
            tool_call_id,
            canonical: Some(appender),
            append_gate: Arc::new(Mutex::new(())),
            pending_utf8: Arc::new(Mutex::new(std::array::from_fn(|_| Vec::new()))),
        }
    }

    pub async fn writer_for(&self, tool_call_id: impl Into<String>) -> LiveToolOutputWriter {
        let tool_call_id = tool_call_id.into();
        self.inner
            .lock()
            .await
            .entry(tool_call_id.clone())
            .or_default();
        LiveToolOutputWriter {
            registry: self.clone(),
            tool_call_id,
            canonical: None,
            append_gate: Arc::new(Mutex::new(())),
            pending_utf8: Arc::new(Mutex::new(std::array::from_fn(|_| Vec::new()))),
        }
    }

    pub async fn plugin_receipt(
        &self,
        tool_call_doc_id: &str,
    ) -> Option<gents_protocol::plugin::PluginExecutionReceipt> {
        self.inner
            .lock()
            .await
            .get(tool_call_doc_id)
            .and_then(|state| state.plugin_receipt.clone())
    }

    pub async fn deferred_writes(&self, tool_call_doc_id: &str) -> Option<DeferredToolWrites> {
        self.inner
            .lock()
            .await
            .get(tool_call_doc_id)
            .and_then(|state| state.deferred_writes.clone())
    }

    pub async fn live_ids(&self) -> Vec<String> {
        self.inner.lock().await.keys().cloned().collect()
    }

    pub async fn snapshot(&self, tool_call_id: &str) -> Option<LiveToolOutputSnapshot> {
        self.inner
            .lock()
            .await
            .contains_key(tool_call_id)
            .then(LiveToolOutputSnapshot::empty)
    }

    pub async fn remove(&self, tool_call_id: &str) {
        self.inner.lock().await.remove(tool_call_id);
    }

    async fn append(&self, tool_call_id: &str, stream: LiveOutputStream, bytes: &[u8]) {
        let _ = (tool_call_id, stream, bytes);
    }

    async fn record_receipt(
        &self,
        tool_call_id: &str,
        stream: LiveOutputStream,
        source: std::ops::Range<u64>,
    ) -> Result<()> {
        if source.is_empty() {
            return Ok(());
        }
        let mut live = self.inner.lock().await;
        let state = live
            .get_mut(tool_call_id)
            .ok_or(LiveOutputError::ReceiptRegistrationMissing)?;
        let receipts = &mut state.receipts[stream.index()];
        let channel_start = receipts
            .last()
            .map(|receipt| receipt.channel_end)
            .unwrap_or(0);
        let bytes = source
            .end
            .checked_sub(source.start)
            .ok_or(LiveOutputError::ReceiptUnderflow)?;
        receipts.push(OutputReceipt {
            channel_start,
            channel_end: channel_start + bytes,
            source_start: source.start,
            source_end: source.end,
        });
        Ok(())
    }

    /// Read the prepared view without consuming it; terminal settlement removes
    /// the invocation only after its durable write commits or is adopted.
    pub async fn prepared_presentation(
        &self,
        tool_call_id: &str,
        expected_text: &str,
    ) -> Result<Option<PayloadPresentation>> {
        let live = self.inner.lock().await;
        let Some(prepared) = live
            .get(tool_call_id)
            .and_then(|state| state.prepared.as_ref())
        else {
            return Ok(None);
        };
        if prepared.expected_text != expected_text {
            return Err(LiveOutputError::PreparedPresentationMismatch);
        }
        Ok(Some(prepared.presentation.clone()))
    }

    pub async fn take_prepared_presentation(
        &self,
        tool_call_id: &str,
        expected_text: &str,
    ) -> Result<Option<PayloadPresentation>> {
        let mut live = self.inner.lock().await;
        let Some(state) = live.get_mut(tool_call_id) else {
            return Ok(None);
        };
        let Some(prepared) = state.prepared.take() else {
            return Ok(None);
        };
        if prepared.expected_text != expected_text {
            return Err(LiveOutputError::PreparedPresentationMismatch);
        }
        Ok(Some(prepared.presentation))
    }
}

impl LiveOutputStream {
    const fn index(self) -> usize {
        match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveToolOutputSnapshot {
    pub combined: LiveOutputStreamSnapshot,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct LiveOutputStreamSnapshot {
    pub bytes: Vec<u8>,
    pub first_offset: u64,
    pub total_bytes_seen: u64,
}

impl LiveToolOutputSnapshot {
    fn empty() -> Self {
        Self {
            combined: LiveOutputStreamSnapshot {
                bytes: Vec::new(),
                first_offset: 0,
                total_bytes_seen: 0,
            },
            stdout_bytes: 0,
            stderr_bytes: 0,
        }
    }
}

#[cfg(test)]
#[path = "live_output_tests.rs"]
mod tests;
