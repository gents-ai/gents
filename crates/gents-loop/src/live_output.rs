use rapidhash::RapidHashMap;
use std::any::{Any, TypeId};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gents_protocol::error_chain::ErrorChain;
use gents_protocol::output::{PayloadPresentation, PresentationPart};
use tokio::sync::Mutex;

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
/// starts carrying stderr bytes; shared verbatim with `gents::background_tools`.
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

    async fn prepare_command_presentation(
        &self,
        tool_call_id: &str,
        expected_text: &str,
        layout: CommandPresentationLayout<'_>,
        stdout: CapturedChannel<'_>,
        stderr: CapturedChannel<'_>,
    ) -> Result<()> {
        let mut live = self.inner.lock().await;
        let state = live
            .get_mut(tool_call_id)
            .ok_or(LiveOutputError::PresentationRegistrationMissing)?;
        if channel_len(&state.receipts[LiveOutputStream::Stdout.index()]) != stdout.raw.len() as u64
            || channel_len(&state.receipts[LiveOutputStream::Stderr.index()])
                != stderr.raw.len() as u64
        {
            return Err(LiveOutputError::CaptureReceiptsIncomplete);
        }
        let json_object = matches!(layout, CommandPresentationLayout::JsonObject { .. });
        let stdout_plan = channel_plan(&stdout, !json_object)?;
        let stderr_plan = channel_plan(&stderr, !json_object)?;
        let mut parts = Vec::new();
        match layout {
            CommandPresentationLayout::JsonObject { metadata_json } => {
                if !(metadata_json.starts_with('{') && metadata_json.ends_with('}')) {
                    return Err(LiveOutputError::MetadataNotObject);
                }
                let metadata_fields = &metadata_json[1..metadata_json.len() - 1];
                push_literal(&mut parts, r#"{"stdout":""#.to_owned());
                append_json_channel(&mut parts, stdout.raw, &stdout_plan, &state.receipts[0])?;
                push_literal(&mut parts, r#"","stderr":""#.to_owned());
                append_json_channel(&mut parts, stderr.raw, &stderr_plan, &state.receipts[1])?;
                push_literal(&mut parts, format!("\",{metadata_fields}}}"));
            }
            CommandPresentationLayout::Labeled {
                head,
                json_string: false,
            } => {
                push_literal(&mut parts, format!("{head}stdout:\n"));
                append_plain_channel(&mut parts, &stdout_plan, &state.receipts[0])?;
                push_literal(&mut parts, "\nstderr:\n".to_owned());
                append_plain_channel(&mut parts, &stderr_plan, &state.receipts[1])?;
            }
            CommandPresentationLayout::Labeled {
                head,
                json_string: true,
            } => {
                push_literal(
                    &mut parts,
                    format!("\"{}", json_fragment(&format!("{head}stdout:\n"))?),
                );
                append_json_channel(&mut parts, stdout.raw, &stdout_plan, &state.receipts[0])?;
                push_literal(&mut parts, json_fragment("\nstderr:\n")?);
                append_json_channel(&mut parts, stderr.raw, &stderr_plan, &state.receipts[1])?;
                push_literal(&mut parts, "\"".to_owned());
            }
        }
        let presentation = PayloadPresentation::Composed { parts };
        state.prepared = Some(PreparedToolPresentation {
            expected_text: expected_text.to_owned(),
            presentation,
        });
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CommandPresentationLayout<'a> {
    JsonObject { metadata_json: &'a str },
    Labeled { head: &'a str, json_string: bool },
}

#[derive(Debug, Clone)]
pub struct CapturedChannel<'a> {
    pub raw: &'a str,
    pub rendered: &'a str,
    pub shown: std::ops::Range<usize>,
}

#[derive(Clone)]
enum ChannelPart {
    Range { start: u64, end: u64 },
    Literal(String),
}

fn channel_len(receipts: &[OutputReceipt]) -> u64 {
    receipts
        .last()
        .map(|receipt| receipt.channel_end)
        .unwrap_or(0)
}

fn channel_plan(channel: &CapturedChannel<'_>, empty_marker: bool) -> Result<Vec<ChannelPart>> {
    let CapturedChannel {
        raw,
        rendered,
        shown,
    } = channel;
    if !(shown.start <= shown.end
        && shown.end <= raw.len()
        && raw.is_char_boundary(shown.start)
        && raw.is_char_boundary(shown.end)
        && (shown.start == 0 || shown.end == raw.len()))
    {
        return Err(LiveOutputError::InvalidTruncationRange);
    }
    let kept = &raw[shown.clone()];
    let (before, after) = if shown.start == 0 {
        if !rendered.starts_with(kept) {
            return Err(LiveOutputError::TruncationPrefixMismatch);
        }
        ("", &rendered[kept.len()..])
    } else {
        if !rendered.ends_with(kept) {
            return Err(LiveOutputError::TruncationSuffixMismatch);
        }
        (&rendered[..rendered.len() - kept.len()], "")
    };
    let mut parts = Vec::new();
    if !before.is_empty() {
        parts.push(ChannelPart::Literal(before.to_owned()));
    }
    if !kept.is_empty() {
        parts.push(ChannelPart::Range {
            start: shown.start as u64,
            end: shown.end as u64,
        });
    }
    if !after.is_empty() {
        parts.push(ChannelPart::Literal(after.to_owned()));
    }
    if parts.is_empty() && empty_marker {
        parts.push(ChannelPart::Literal("(empty)".to_owned()));
    }
    Ok(parts)
}

fn push_literal(parts: &mut Vec<PresentationPart>, text: String) {
    if !text.is_empty() {
        parts.push(PresentationPart::Literal { text });
    }
}

fn append_plain_channel(
    output: &mut Vec<PresentationPart>,
    plan: &[ChannelPart],
    receipts: &[OutputReceipt],
) -> Result<()> {
    for part in plan {
        match part {
            ChannelPart::Range { start, end } => {
                append_channel_range(output, receipts, *start, *end)?
            }
            ChannelPart::Literal(text) => push_literal(output, text.clone()),
        }
    }
    Ok(())
}

fn append_channel_range(
    output: &mut Vec<PresentationPart>,
    receipts: &[OutputReceipt],
    start: u64,
    end: u64,
) -> Result<()> {
    let mut cursor = start;
    for receipt in receipts {
        let from = cursor.max(receipt.channel_start);
        let until = end.min(receipt.channel_end);
        if from >= until {
            continue;
        }
        let source_start = receipt.source_start + (from - receipt.channel_start);
        let source_end = receipt.source_start + (until - receipt.channel_start);
        if source_end > receipt.source_end {
            return Err(LiveOutputError::ReceiptRangeExceedsChunk);
        }
        output.push(PresentationPart::OutputRange {
            start_byte: source_start,
            end_byte: source_end,
        });
        cursor = until;
    }
    if cursor != end {
        return Err(LiveOutputError::ReceiptGap);
    }
    Ok(())
}

fn append_json_channel(
    output: &mut Vec<PresentationPart>,
    raw: &str,
    plan: &[ChannelPart],
    receipts: &[OutputReceipt],
) -> Result<()> {
    for part in plan {
        match part {
            ChannelPart::Literal(text) => push_literal(output, json_fragment(text)?),
            ChannelPart::Range { start, end } => {
                let start = usize::try_from(*start)?;
                let end = usize::try_from(*end)?;
                let selected = &raw[start..end];
                for (offset, ch) in selected.char_indices() {
                    let next = offset + ch.len_utf8();
                    let encoded = json_fragment(&ch.to_string())?;
                    if encoded == ch.to_string() {
                        append_channel_range(
                            output,
                            receipts,
                            start as u64 + offset as u64,
                            start as u64 + next as u64,
                        )?;
                    } else {
                        push_literal(output, encoded);
                    }
                }
            }
        }
    }
    Ok(())
}

fn json_fragment(text: &str) -> Result<String> {
    let encoded = serde_json::to_string(text)?;
    Ok(encoded[1..encoded.len() - 1].to_owned())
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
pub struct LiveToolOutputWriter {
    registry: LiveToolOutputRegistry,
    tool_call_id: String,
    canonical: Option<Arc<dyn CanonicalOutputAppender>>,
    append_gate: Arc<Mutex<()>>,
    pending_utf8: Arc<Mutex<[Vec<u8>; 2]>>,
}

impl LiveToolOutputWriter {
    /// The physical tool call this writer registers output for.
    pub fn tool_call_doc_id(&self) -> &str {
        &self.tool_call_id
    }

    pub async fn record_plugin_receipt(
        &self,
        receipt: gents_protocol::plugin::PluginExecutionReceipt,
    ) -> Result<()> {
        let mut states = self.registry.inner.lock().await;
        let state = states
            .get_mut(&self.tool_call_id)
            .ok_or(LiveOutputError::PluginReceiptUnregistered)?;
        if !state
            .plugin_receipt
            .as_ref()
            .is_none_or(|existing| existing == &receipt)
        {
            return Err(LiveOutputError::PluginReceiptConflict);
        }
        state.plugin_receipt = Some(receipt);
        Ok(())
    }

    pub async fn record_deferred_writes(&self, writes: DeferredToolWrites) -> Result<()> {
        let mut states = self.registry.inner.lock().await;
        let state = states
            .get_mut(&self.tool_call_id)
            .ok_or(LiveOutputError::DeferredWritesUnregistered)?;
        match &mut state.deferred_writes {
            Some(existing) => existing.0.extend(writes.0),
            None => state.deferred_writes = Some(writes),
        }
        Ok(())
    }

    pub async fn prepare_command_presentation(
        &self,
        expected_text: &str,
        layout: CommandPresentationLayout<'_>,
        stdout: CapturedChannel<'_>,
        stderr: CapturedChannel<'_>,
    ) -> Result<()> {
        if self.canonical.is_none() {
            return Err(LiveOutputError::CanonicalWriterRequired);
        }
        self.flush_pending().await;
        self.registry
            .prepare_command_presentation(&self.tool_call_id, expected_text, layout, stdout, stderr)
            .await
    }

    pub async fn append(&self, stream: LiveOutputStream, bytes: &[u8]) {
        if let Some(binding) = &self.canonical {
            let _serial = self.append_gate.lock().await;
            let text = {
                let mut pending = self.pending_utf8.lock().await;
                let pending = &mut pending[stream.index()];
                pending.extend_from_slice(bytes);
                drain_utf8(pending, false)
            };
            self.commit(binding.as_ref(), stream, &text).await;
            return;
        }
        self.registry
            .append(&self.tool_call_id, stream, bytes)
            .await;
    }

    async fn flush_pending(&self) {
        let Some(binding) = &self.canonical else {
            return;
        };
        let _serial = self.append_gate.lock().await;
        for stream in [LiveOutputStream::Stdout, LiveOutputStream::Stderr] {
            let text = {
                let mut pending = self.pending_utf8.lock().await;
                drain_utf8(&mut pending[stream.index()], true)
            };
            self.commit(binding.as_ref(), stream, &text).await;
        }
    }

    async fn commit(
        &self,
        binding: &dyn CanonicalOutputAppender,
        stream: LiveOutputStream,
        text: &str,
    ) {
        if text.is_empty() {
            return;
        }
        match binding.append(text).await {
            Ok(source) => {
                if let Err(error) = self
                    .registry
                    .record_receipt(&self.tool_call_id, stream, source)
                    .await
                {
                    tracing::error!(tool_call_doc_id = %self.tool_call_id,
                        error = %ErrorChain(&error),
                        "canonical tool receipt recording failed");
                }
            }
            Err(error) => tracing::error!(tool_call_doc_id = %self.tool_call_id,
                error = %ErrorChain(&error),
                "canonical tool output append failed"),
        }
    }
}

fn drain_utf8(pending: &mut Vec<u8>, end_of_stream: bool) -> String {
    let mut text = String::new();
    let mut consumed = 0;
    while consumed < pending.len() {
        match std::str::from_utf8(&pending[consumed..]) {
            Ok(valid) => {
                text.push_str(valid);
                consumed = pending.len();
            }
            Err(error) => {
                let valid = error.valid_up_to();
                text.push_str(
                    std::str::from_utf8(&pending[consumed..consumed + valid])
                        .expect("valid_up_to is valid UTF-8"),
                );
                consumed += valid;
                match error.error_len() {
                    Some(invalid) => {
                        text.push(char::REPLACEMENT_CHARACTER);
                        consumed += invalid;
                    }
                    None if end_of_stream => {
                        text.push(char::REPLACEMENT_CHARACTER);
                        consumed = pending.len();
                    }
                    None => break,
                }
            }
        }
    }
    pending.drain(..consumed);
    text
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
