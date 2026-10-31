use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use tokio::sync::Mutex;

pub(crate) mod auxiliary;
#[cfg(test)]
mod auxiliary_tests;
pub(crate) mod canonical;
#[cfg(test)]
mod canonical_tests;
pub(crate) mod native_encoding;

#[derive(Clone, Debug)]
pub struct AcceptedToolCall {
    pub(crate) tool_call_doc_id: String,
    pub(crate) request_doc_id: String,
    pub(crate) session_id: String,
    pub(crate) accepted_header_doc_id: String,
    pub(crate) message_sequence: u32,
    pub(crate) id: String,
    pub(crate) call_id: Option<String>,
    pub(crate) tool_name: String,
    pub(crate) execution_generation: String,
    pub(crate) arguments: gents_protocol::output::PayloadRef,
    pub(crate) await_mode: crate::tools::call::lifecycle::AwaitMode,
}

/// Maximum UTF-8 byte length of the reasoning preview persisted while a
/// response is streaming. Consumers that reconstruct preview rollover must
/// use this same bound.
pub const MAX_LIVE_REASONING_BYTES: usize = 4 * 1024;

/// Multiple of the stream batch interval at which reasoning-only growth is
/// flushed. Every flush appends a permanent `AgentOutputSegment`, and
/// reasoning dominates streamed bytes for reasoning models, so at the plain
/// batch interval a response stores one document per streaming second.
/// Visible text, tool calls and the first reasoning bytes keep their cadence.
pub const REASONING_BATCH_FACTOR: u32 = 5;

use gents_loop::stream_writer::PendingOutput;
pub use gents_loop::stream_writer::StreamWriter;

pub struct DefraStreamWriter {
    node: Arc<EmbeddedNode>,
    batch_interval: Duration,
    buffers: Mutex<RapidHashMap<String, StreamBuffer>>,
    published: Mutex<RapidHashMap<String, String>>,
    provider_tails: Mutex<RapidHashMap<String, ProviderTail>>,
}

#[derive(Default)]
struct ProviderTail {
    turn: usize,
    attempt: u32,
    streams: Vec<native_encoding::EncodedStream>,
    next_ordinal: u32,
    capture_scope: Option<gents_protocol::rendered_request::CaptureScope>,
    pending_flush: Option<PreparedProviderFlush>,
}

/// A native commit can outlive its awaiting caller. Retain the exact record
/// and stream checkpoint until acknowledgement; regenerating its timestamp
/// would violate the canonical append owner's immutable replay contract.
struct PreparedProviderFlush {
    segment: gents_protocol::output::OutputSegment,
    streams: Vec<native_encoding::EncodedStream>,
}

struct StreamBuffer {
    current: StreamBufferSnapshot,
    last_flush_at: Instant,
    persisted: StreamBufferSnapshot,
}

impl StreamBuffer {
    fn flush_interval(&self, batch_interval: Duration) -> Duration {
        let reasoning_only = self.current.content_bytes == self.persisted.content_bytes
            && self.current.visible_progress_seq == self.persisted.visible_progress_seq;
        if reasoning_only {
            batch_interval.saturating_mul(REASONING_BATCH_FACTOR)
        } else {
            batch_interval
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct StreamBufferSnapshot {
    content_bytes: usize,
    reasoning: String,
    reasoning_progress_seq: usize,
    visible_progress_seq: usize,
    reasoning_marked: bool,
}

impl StreamBufferSnapshot {
    fn record_content(&mut self, tokens: &str) {
        self.content_bytes += tokens.len();
    }

    fn record_reasoning(&mut self, reasoning: &str) {
        append_live_reasoning_preview(&mut self.reasoning, reasoning);
        self.mark(PendingOutput::Reasoning);
    }

    fn mark(&mut self, output: PendingOutput) {
        match output {
            PendingOutput::Reasoning => {
                self.reasoning_progress_seq = self.reasoning_progress_seq.wrapping_add(1);
                self.reasoning_marked = true;
            }
            PendingOutput::Visible => {
                self.visible_progress_seq = self.visible_progress_seq.wrapping_add(1);
            }
        }
    }

    fn first_visible_since(&self, persisted: &Self) -> bool {
        (persisted.content_bytes == 0 && self.content_bytes > 0)
            || (!persisted.reasoning_marked && self.reasoning_marked)
    }
}

impl DefraStreamWriter {
    pub(crate) async fn publish_authored_message(
        &self,
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
        key: &str,
        message: &gents_protocol::message::Message,
    ) -> Result<String> {
        self.publish_authored_message_with_time(lifecycle, key, message, None)
            .await
    }

    pub(crate) async fn publish_authored_message_with_time(
        &self,
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
        key: &str,
        message: &gents_protocol::message::Message,
        fixture_now: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<String> {
        use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter, SegmentRun};
        let request = lifecycle.request();
        let encoded = Arc::new(native_encoding::encode_native_message(message)?);
        let mut payload = String::new();
        let mut runs = Vec::new();
        for (index, stream) in encoded.streams.iter().enumerate() {
            payload.push_str(&stream.payload);
            runs.push(SegmentRun {
                stream: u32::try_from(index)?,
                bytes: u32::try_from(stream.payload.len())?,
                declaration: Some(stream.declaration.clone()),
            });
        }
        let source = OutputSource::Authored {
            key: key.to_owned(),
        };
        let segment = OutputSegment {
            node_did: request.node_did.clone(),
            requester_did: request.requester_did.clone(),
            session_id: request.session_id.clone(),
            request_doc_id: request.doc_id.clone(),
            source: source.clone(),
            writer: OutputWriter::RequestExecution {
                execution_generation: lifecycle.execution_generation()?.to_owned(),
            },
            ordinal: (!runs.is_empty()).then_some(0),
            runs,
            payload,
            close: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let plan = canonical::ProviderPublicationPlan {
            final_flush: Some(segment),
            message_key: crate::session::canonical_rows::authored_message_key(&request.doc_id, key),
            encoded,
            expected: Arc::new(message.clone()),
            tool_deadline_at: lifecycle
                .claimed_deadline_at()
                .context("authored publication is missing request deadline")?
                .to_rfc3339(),
            background_calls: Vec::new(),
            consumes_folded: crate::request::lifecycle::queue::FoldedConsumption::for_key(
                request,
                key,
                lifecycle.folded_selection(),
            ),
        };
        let published = canonical::publish_provider_turn_with_time(
            &self.node,
            lifecycle.execution_generation()?,
            plan,
            fixture_now,
        )
        .await?;
        Ok(published.message_doc_id)
    }

    pub(crate) async fn start_provider_attempt(
        &self,
        request_doc_id: &str,
        turn: usize,
        attempt: u32,
        capture_scope: gents_protocol::rendered_request::CaptureScope,
    ) {
        self.provider_tails.lock().await.insert(
            request_doc_id.to_owned(),
            ProviderTail {
                turn,
                attempt,
                capture_scope: Some(capture_scope),
                ..ProviderTail::default()
            },
        );
    }

    pub(crate) async fn flush_native_partial(
        &self,
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
        message: &gents_protocol::message::Message,
    ) -> Result<bool> {
        self.flush_owned_partial(
            lifecycle.request(),
            lifecycle.execution_generation()?,
            message,
        )
        .await
    }

    pub(crate) async fn flush_owned_partial(
        &self,
        request: &crate::request::watcher::AgentRequest,
        generation: &str,
        message: &gents_protocol::message::Message,
    ) -> Result<bool> {
        use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter};
        let encoded = native_encoding::encode_native_message(message)?;
        let mut tails = self.provider_tails.lock().await;
        let tail = tails
            .get_mut(&request.doc_id)
            .context("provider attempt is not active")?;
        let acknowledged = self.acknowledge_provider_flush(tail, generation).await?;
        let turn = tail.turn;
        let attempt = tail.attempt;
        let capture_scope = tail
            .capture_scope
            .context("provider attempt omitted capture scope")?;
        let ordinal = tail.next_ordinal;
        let delta = provider_flush_delta(&tail.streams, &encoded)?;
        if !delta.grew {
            return Ok(acknowledged);
        }
        let segment = OutputSegment {
            node_did: request.node_did.clone(),
            requester_did: request.requester_did.clone(),
            session_id: request.session_id.clone(),
            request_doc_id: request.doc_id.clone(),
            source: OutputSource::ProviderTurn {
                scope: capture_scope,
                turn_index: u32::try_from(turn)?,
                attempt,
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: generation.to_owned(),
            },
            ordinal: Some(ordinal),
            runs: delta.runs,
            payload: delta.payload,
            close: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        tail.pending_flush = Some(PreparedProviderFlush {
            segment,
            streams: encoded.streams,
        });
        self.acknowledge_provider_flush(tail, generation).await
    }

    async fn acknowledge_provider_flush(
        &self,
        tail: &mut ProviderTail,
        generation: &str,
    ) -> Result<bool> {
        let Some(pending) = tail.pending_flush.as_ref() else {
            return Ok(false);
        };
        anyhow::ensure!(
            pending.segment.ordinal == Some(tail.next_ordinal),
            "pending provider flush disagrees with its acknowledged cursor"
        );
        canonical::append_provider_segment(&self.node, generation, &pending.segment).await?;
        let pending = tail
            .pending_flush
            .take()
            .context("provider flush lost its prepared record")?;
        tail.streams = pending.streams;
        tail.next_ordinal += 1;
        Ok(true)
    }

    pub(crate) async fn close_provider_attempt(
        &self,
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
        turn: usize,
        attempt: u32,
        close: canonical::ProviderAttemptClose,
    ) -> Result<()> {
        self.close_owned_attempt(
            lifecycle.request(),
            lifecycle.execution_generation()?,
            turn,
            attempt,
            close,
        )
        .await
    }

    pub(crate) async fn close_owned_attempt(
        &self,
        request: &crate::request::watcher::AgentRequest,
        generation: &str,
        turn: usize,
        attempt: u32,
        close: canonical::ProviderAttemptClose,
    ) -> Result<()> {
        use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter};
        let mut tails = self.provider_tails.lock().await;
        let tail = tails
            .get_mut(&request.doc_id)
            .filter(|tail| tail.turn == turn && tail.attempt == attempt)
            .context("provider close has no exact active attempt")?;
        self.acknowledge_provider_flush(tail, generation).await?;
        let scope = tail
            .capture_scope
            .context("provider close omitted capture scope")?;
        let segment = OutputSegment {
            node_did: request.node_did.clone(),
            requester_did: request.requester_did.clone(),
            session_id: request.session_id.clone(),
            request_doc_id: request.doc_id.clone(),
            source: OutputSource::ProviderTurn {
                scope,
                turn_index: u32::try_from(turn)?,
                attempt,
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: generation.to_owned(),
            },
            ordinal: None,
            runs: Vec::new(),
            payload: String::new(),
            close: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let partial_message_doc_id =
            canonical::close_provider_attempt(&self.node, generation, &segment, close).await?;
        tails.remove(&request.doc_id);
        drop(tails);
        if let Some(message_doc_id) = partial_message_doc_id {
            self.published
                .lock()
                .await
                .insert(request.doc_id.clone(), message_doc_id);
        }
        Ok(())
    }

    pub(crate) async fn publish_native_turn(
        &self,
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
        turn: usize,
        attempt: u32,
        message: &gents_protocol::message::Message,
    ) -> Result<canonical::PublishedProviderTurn> {
        use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter};

        let request = lifecycle.request();
        let encoded = std::sync::Arc::new(native_encoding::encode_native_message(message)?);
        let expected = std::sync::Arc::new(message.clone());
        let mut tails = self.provider_tails.lock().await;
        let tail = tails
            .get_mut(&request.doc_id)
            .filter(|tail| tail.turn == turn && tail.attempt == attempt)
            .context("provider acceptance has no exact active attempt")?;
        self.acknowledge_provider_flush(tail, lifecycle.execution_generation()?)
            .await?;
        let created_at = chrono::Utc::now().to_rfc3339();
        let capture_scope = tail
            .capture_scope
            .context("provider acceptance omitted capture scope")?;
        let source = OutputSource::ProviderTurn {
            scope: capture_scope,
            turn_index: u32::try_from(turn).context("provider turn exceeds u32")?,
            attempt,
        };
        let delta = provider_flush_delta(&tail.streams, &encoded)?;
        let segment = if delta.grew {
            OutputSegment {
                node_did: request.node_did.clone(),
                requester_did: request.requester_did.clone(),
                session_id: request.session_id.clone(),
                request_doc_id: request.doc_id.clone(),
                source: source.clone(),
                writer: OutputWriter::RequestExecution {
                    execution_generation: lifecycle.execution_generation()?.to_owned(),
                },
                ordinal: Some(tail.next_ordinal),
                runs: delta.runs,
                payload: delta.payload,
                close: None,
                created_at,
            }
        } else {
            OutputSegment {
                node_did: request.node_did.clone(),
                requester_did: request.requester_did.clone(),
                session_id: request.session_id.clone(),
                request_doc_id: request.doc_id.clone(),
                source: source.clone(),
                writer: OutputWriter::RequestExecution {
                    execution_generation: lifecycle.execution_generation()?.to_owned(),
                },
                ordinal: None,
                runs: Vec::new(),
                payload: String::new(),
                close: None,
                created_at,
            }
        };
        let published = canonical::publish_provider_turn(
            &self.node,
            lifecycle.execution_generation()?,
            canonical::ProviderPublicationPlan {
                final_flush: Some(segment),
                message_key: format!(
                    "provider:{}:{}",
                    request.doc_id,
                    serde_json::to_string(&source)?
                ),
                encoded: encoded.clone(),
                expected,
                tool_deadline_at: lifecycle
                    .claimed_deadline_at()
                    .context("accepted tool call is missing its request deadline")?
                    .to_rfc3339(),
                background_calls: canonical::session_message_call_ids(message),
                consumes_folded: None,
            },
        )
        .await?;
        let tail = tails
            .get_mut(&request.doc_id)
            .filter(|tail| tail.turn == turn && tail.attempt == attempt)
            .context("provider attempt ended during acceptance")?;
        tail.streams = std::sync::Arc::unwrap_or_clone(encoded).streams;
        tail.next_ordinal += u32::from(delta.grew);
        drop(tails);
        self.published
            .lock()
            .await
            .insert(request.doc_id.clone(), published.message_doc_id.clone());
        Ok(published)
    }

    pub(crate) async fn terminal_output(
        &self,
        request_doc_id: &str,
    ) -> gents_protocol::output::TerminalOutput {
        self.published
            .lock()
            .await
            .get(request_doc_id)
            .cloned()
            .map_or(
                gents_protocol::output::TerminalOutput::NoMessage,
                |message_doc_id| gents_protocol::output::TerminalOutput::Message { message_doc_id },
            )
    }

    pub(crate) async fn next_flush_deadline(&self, doc_id: &str) -> Option<tokio::time::Instant> {
        let buffers = self.buffers.lock().await;
        let buffer = buffers.get(doc_id)?;
        if buffer.current == buffer.persisted {
            return None;
        }
        buffer
            .last_flush_at
            .checked_add(buffer.flush_interval(self.batch_interval))
            .map(tokio::time::Instant::from_std)
    }

    pub fn new(node: Arc<EmbeddedNode>, _node_did: &str, batch_interval: Duration) -> Self {
        Self {
            node,
            batch_interval,
            buffers: Mutex::new(RapidHashMap::new()),
            published: Mutex::new(RapidHashMap::new()),
            provider_tails: Mutex::new(RapidHashMap::new()),
        }
    }

    pub(crate) async fn initialize_request_buffer(&self, request_doc_id: &str) -> Result<()> {
        self.buffers.lock().await.insert(
            request_doc_id.to_owned(),
            StreamBuffer {
                current: StreamBufferSnapshot::default(),
                last_flush_at: Instant::now(),
                persisted: StreamBufferSnapshot::default(),
            },
        );
        Ok(())
    }

    async fn flush_snapshot(&self, doc_id: &str, _snapshot: &StreamBufferSnapshot) -> Result<bool> {
        let Some(snapshot) = self.pending_snapshot(doc_id, true).await? else {
            return Ok(false);
        };
        if let Some(buffer) = self.buffers.lock().await.get_mut(doc_id) {
            buffer.persisted = snapshot.clone();
            buffer.last_flush_at = Instant::now();
        }
        Ok(true)
    }

    async fn pending_snapshot(
        &self,
        doc_id: &str,
        force: bool,
    ) -> Result<Option<StreamBufferSnapshot>> {
        let mut buffers = self.buffers.lock().await;
        let buf = buffers
            .get_mut(doc_id)
            .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
        let first_visible_content = buf.current.first_visible_since(&buf.persisted);
        if !force
            && !first_visible_content
            && buf.last_flush_at.elapsed() < buf.flush_interval(self.batch_interval)
        {
            return Ok(None);
        }
        let snapshot = buf.current.clone();
        Ok((snapshot != buf.persisted).then_some(snapshot))
    }

    pub async fn reset_tail(&self, doc_id: &str) -> Result<()> {
        let mut buffers = self.buffers.lock().await;
        let buf = buffers
            .get_mut(doc_id)
            .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
        if buf.current.content_bytes == 0
            && !buf.current.reasoning_marked
            && buf.persisted.content_bytes == 0
            && !buf.persisted.reasoning_marked
        {
            return Ok(());
        }
        buf.current.content_bytes = 0;
        buf.current.reasoning.clear();
        buf.current.reasoning_marked = false;
        buf.persisted = buf.current.clone();
        buf.last_flush_at = Instant::now();
        Ok(())
    }
}

#[cfg(test)]
impl DefraStreamWriter {
    /// Moves the batch clock as if `elapsed` had passed since the last flush.
    pub(crate) async fn advance_batch_clock(&self, doc_id: &str, elapsed: Duration) {
        if let Some(buffer) = self.buffers.lock().await.get_mut(doc_id) {
            buffer.last_flush_at = buffer
                .last_flush_at
                .checked_sub(elapsed)
                .expect("batch clock moved before process start");
        }
    }
}

impl gents_loop::stream_writer::CanonicalStreamWriter<crate::request::lifecycle::RequestLifecycle>
    for DefraStreamWriter
{
    type AcceptedToolCall = AcceptedToolCall;

    async fn publish_authored_message(
        &self,
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
        key: &str,
        message: &gents_protocol::message::Message,
    ) -> Result<String, Self::Error> {
        Ok(DefraStreamWriter::publish_authored_message(self, lifecycle, key, message).await?)
    }

    async fn try_publish_steering_message(
        &self,
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
        key: &str,
        message: &gents_protocol::message::Message,
    ) -> Result<Option<String>, Self::Error> {
        match DefraStreamWriter::publish_authored_message(self, lifecycle, key, message).await {
            Ok(id) => Ok(Some(id)),
            Err(error) if error.is::<crate::request::lifecycle::queue::PendingInputChanged>() => {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn finish_natural_turn(
        &self,
        lifecycle: &mut crate::request::lifecycle::RequestLifecycle,
        message_doc_id: &str,
        pending_request_doc_ids: &[String],
    ) -> Result<bool, Self::Error> {
        Ok(lifecycle
            .finish_natural_turn(message_doc_id, pending_request_doc_ids)
            .await?)
    }

    async fn start_provider_attempt(
        &self,
        request_doc_id: &str,
        turn: usize,
        attempt: u32,
        capture_scope: gents_protocol::rendered_request::CaptureScope,
    ) {
        DefraStreamWriter::start_provider_attempt(
            self,
            request_doc_id,
            turn,
            attempt,
            capture_scope,
        )
        .await
    }

    async fn flush_native_partial(
        &self,
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
        message: &gents_protocol::message::Message,
    ) -> Result<bool, Self::Error> {
        Ok(DefraStreamWriter::flush_native_partial(self, lifecycle, message).await?)
    }

    async fn close_provider_attempt(
        &self,
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
        turn: usize,
        attempt: u32,
        close: gents_loop::stream_writer::ProviderAttemptClose,
    ) -> Result<(), Self::Error> {
        let close = match close {
            gents_loop::stream_writer::ProviderAttemptClose::Retracted => {
                canonical::ProviderAttemptClose::Retracted
            }
            gents_loop::stream_writer::ProviderAttemptClose::Partial => {
                canonical::ProviderAttemptClose::Partial
            }
        };
        Ok(
            DefraStreamWriter::close_provider_attempt(self, lifecycle, turn, attempt, close)
                .await?,
        )
    }

    async fn publish_native_turn(
        &self,
        lifecycle: &crate::request::lifecycle::RequestLifecycle,
        turn: usize,
        attempt: u32,
        message: &gents_protocol::message::Message,
    ) -> Result<gents_loop::stream_writer::CanonicalPublishedTurn<AcceptedToolCall>, Self::Error>
    {
        let published =
            DefraStreamWriter::publish_native_turn(self, lifecycle, turn, attempt, message).await?;
        Ok(gents_loop::stream_writer::CanonicalPublishedTurn {
            message_doc_id: published.message_doc_id,
            accepted_tools: published.accepted_tools,
        })
    }
}

impl StreamWriter for DefraStreamWriter {
    type Error = crate::runtime::agent::stream_processor::RuntimeSeamError;

    async fn mark_pending_output(
        &self,
        doc_id: &str,
        output: PendingOutput,
    ) -> Result<bool, Self::Error> {
        self.buffers
            .lock()
            .await
            .get_mut(doc_id)
            .context("no output buffer for audit")?
            .current
            .mark(output);
        let Some(snapshot) = self.pending_snapshot(doc_id, false).await? else {
            return Ok(false);
        };
        Ok(self.flush_snapshot(doc_id, &snapshot).await?)
    }

    async fn write_tokens(&self, doc_id: &str, tokens: &str) -> Result<bool, Self::Error> {
        Ok(DefraStreamWriter::write_tokens(self, doc_id, tokens).await?)
    }

    async fn write_reasoning(&self, doc_id: &str, reasoning: &str) -> Result<bool, Self::Error> {
        Ok(DefraStreamWriter::write_reasoning(self, doc_id, reasoning).await?)
    }

    async fn flush_pending(&self, doc_id: &str) -> Result<bool, Self::Error> {
        Ok(DefraStreamWriter::flush_pending(self, doc_id).await?)
    }

    async fn next_flush_deadline(&self, doc_id: &str) -> Option<tokio::time::Instant> {
        DefraStreamWriter::next_flush_deadline(self, doc_id).await
    }

    async fn reset_tail(&self, doc_id: &str) -> Result<(), Self::Error> {
        Ok(DefraStreamWriter::reset_tail(self, doc_id).await?)
    }
}

impl DefraStreamWriter {
    async fn write_tokens(&self, doc_id: &str, tokens: &str) -> Result<bool> {
        if tokens.is_empty() {
            return Ok(false);
        }
        {
            let mut buffers = self.buffers.lock().await;
            let buf = buffers
                .get_mut(doc_id)
                .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
            buf.current.record_content(tokens);
        }

        let snapshot = self.pending_snapshot(doc_id, false).await?;

        let Some(snapshot) = snapshot else {
            return Ok(false);
        };

        self.flush_snapshot(doc_id, &snapshot).await
    }

    async fn write_reasoning(&self, doc_id: &str, reasoning: &str) -> Result<bool> {
        if reasoning.is_empty() {
            return Ok(false);
        }
        {
            let mut buffers = self.buffers.lock().await;
            let buf = buffers
                .get_mut(doc_id)
                .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
            buf.current.record_reasoning(reasoning);
        }

        let snapshot = self.pending_snapshot(doc_id, false).await?;

        let Some(snapshot) = snapshot else {
            return Ok(false);
        };

        self.flush_snapshot(doc_id, &snapshot).await
    }

    async fn flush_pending(&self, doc_id: &str) -> Result<bool> {
        let snapshot = self.pending_snapshot(doc_id, true).await?;
        let Some(snapshot) = snapshot else {
            return Ok(false);
        };
        self.flush_snapshot(doc_id, &snapshot).await
    }
}

struct ProviderStreamDelta {
    runs: Vec<gents_protocol::output::SegmentRun>,
    payload: String,
    grew: bool,
}

fn provider_flush_delta(
    prior: &[native_encoding::EncodedStream],
    encoded: &native_encoding::EncodedNativeMessage,
) -> Result<ProviderStreamDelta> {
    use gents_protocol::output::SegmentRun;
    let mut payload = String::new();
    let mut runs = Vec::new();
    for (index, stream) in encoded.streams.iter().enumerate() {
        let old = prior.get(index);
        if let Some(old) = old {
            anyhow::ensure!(
                old.declaration == stream.declaration && stream.payload.starts_with(&old.payload),
                "provider stream rewrote acknowledged bytes"
            );
        }
        let offset = old.map_or(0, |value| value.payload.len());
        let delta = &stream.payload[offset..];
        if old.is_none() || !delta.is_empty() {
            payload.push_str(delta);
            runs.push(SegmentRun {
                stream: u32::try_from(index)?,
                bytes: u32::try_from(delta.len())?,
                declaration: old.is_none().then(|| stream.declaration.clone()),
            });
        }
    }
    anyhow::ensure!(
        encoded.streams.len() >= prior.len(),
        "provider stream structure shrank"
    );
    Ok(ProviderStreamDelta {
        grew: !runs.is_empty(),
        payload,
        runs,
    })
}

fn append_live_reasoning_preview(buffer: &mut String, reasoning: &str) {
    if reasoning.len() >= MAX_LIVE_REASONING_BYTES {
        buffer.clear();
        buffer.push_str(tail_window(reasoning, MAX_LIVE_REASONING_BYTES));
        return;
    }

    trim_string_to_tail_bytes(buffer, MAX_LIVE_REASONING_BYTES - reasoning.len());
    buffer.push_str(reasoning);
}

fn trim_string_to_tail_bytes(buffer: &mut String, max_bytes: usize) {
    if buffer.len() <= max_bytes {
        return;
    }

    let mut start = buffer.len() - max_bytes;
    while !buffer.is_char_boundary(start) {
        start += 1;
    }
    buffer.drain(..start);
}

pub(crate) fn tail_window(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut start = value.len() - max_bytes;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}
