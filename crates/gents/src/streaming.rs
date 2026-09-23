use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use tokio::sync::Mutex;

pub(crate) mod canonical;
#[cfg(test)]
mod canonical_tests;
pub(crate) mod native_encoding;

/// Exact physical binding returned by accepted provider publication. Tool
/// execution adopts this row; it never recreates provider intent.
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
    pub(crate) delegated_input: Option<gents_protocol::output::DelegatedToolInput>,
    /// Present only when the hook validated a `spawn_subagent` invocation
    /// before its provider turn was accepted.  These are immutable bridge
    /// genesis facts, not dispatch-time metadata.
    pub(crate) spawn_admission: Option<SpawnAdmissionPlan>,
}

/// Immutable child provenance prepared by the hook before provider
/// publication.  The provider header remains the only tool invocation; this
/// merely makes the bridge's child edge available when that header creates its
/// pending lifecycle row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnAdmissionPlan {
    pub(crate) tool_call_id: String,
    pub(crate) child_request_id: String,
    pub(crate) spawn_target_did: String,
    /// Exact immutable behavior route selected before provider publication.
    /// Remote materializers must never recover this from provider arguments.
    pub(crate) spawn_behavior_id: String,
    /// Authenticated parent workspace snapshot.  This is bridge provenance,
    /// not a tool argument or workspace grant.
    pub(crate) delegated_workspace: Option<gents_protocol::output::DelegatedWorkspace>,
    pub(crate) await_mode: crate::tool_call_lifecycle::AwaitMode,
}

/// Maximum UTF-8 byte length of the reasoning preview persisted while a
/// response is streaming. Consumers that reconstruct preview rollover must
/// use this same bound.
pub const MAX_LIVE_REASONING_BYTES: usize = 4 * 1024;

pub use gents_loop::stream_writer::StreamWriter;

pub struct DefraStreamWriter {
    node: Arc<EmbeddedNode>,
    batch_interval: Duration,
    buffers: Mutex<HashMap<String, StreamBuffer>>,
    published: Mutex<HashMap<String, String>>,
    provider_tails: Mutex<HashMap<String, ProviderTail>>,
}

#[derive(Default)]
struct ProviderTail {
    turn: usize,
    attempt: u32,
    streams: Vec<native_encoding::EncodedStream>,
    next_ordinal: u32,
    capture_scope: Option<gents_protocol::rendered_request::CaptureScope>,
}

struct StreamBuffer {
    current: StreamBufferSnapshot,
    last_flush_at: Instant,
    persisted: StreamBufferSnapshot,
}

/// Uncommitted stream state versus the persisted snapshot. It holds no
/// growing content text: content bookkeeping is a byte counter because
/// content only grows between resets, so counter inequality is exactly text
/// inequality. Reasoning keeps its bounded preview text (capped at
/// `MAX_LIVE_REASONING_BYTES`) because an oversized arrival replaces the
/// preview wholesale; `reasoning_progress_seq` still marks arrivals whose
/// capped preview text is identical to the persisted preview.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct StreamBufferSnapshot {
    content_bytes: usize,
    reasoning: String,
    reasoning_progress_seq: usize,
}

impl StreamBufferSnapshot {
    /// Record a content delta. Callers gate empty deltas.
    fn record_content(&mut self, tokens: &str) {
        self.content_bytes += tokens.len();
    }

    /// Record a reasoning arrival against the bounded preview. Arrivals whose
    /// capped preview text is unchanged still advance the progress sequence.
    fn record_reasoning(&mut self, reasoning: &str) {
        append_live_reasoning_preview(&mut self.reasoning, reasoning);
        self.reasoning_progress_seq = self.reasoning_progress_seq.saturating_add(1);
    }

    /// Whether content or reasoning became visible against the persisted
    /// snapshot for the first time in this stream epoch.
    fn first_visible_since(&self, persisted: &Self) -> bool {
        (persisted.content_bytes == 0 && self.content_bytes > 0)
            || (persisted.reasoning.is_empty() && !self.reasoning.is_empty())
    }
}

impl DefraStreamWriter {
    pub(crate) async fn publish_authored_message(
        &self,
        lifecycle: &crate::lifecycle::RequestLifecycle,
        key: &str,
        message: &gents_protocol::message::Message,
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
            agent_did: request.agent_did.clone(),
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
        let published = canonical::publish_provider_turn(
            &self.node,
            lifecycle.execution_generation()?,
            canonical::ProviderPublicationPlan {
                final_flush: Some(segment),
                message_key: crate::session::canonical_rows::authored_message_key(
                    &request.doc_id,
                    key,
                ),
                encoded,
                expected: Arc::new(message.clone()),
                tool_deadline_at: lifecycle
                    .claimed_deadline_at()
                    .context("authored publication is missing request deadline")?
                    .to_rfc3339(),
                spawn_admissions: Vec::new(),
            },
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
        lifecycle: &crate::lifecycle::RequestLifecycle,
        message: &gents_protocol::message::Message,
    ) -> Result<bool> {
        use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter};
        let request = lifecycle.request();
        let encoded = native_encoding::encode_native_message(message)?;
        // The tail lock is held across the append so a concurrent acceptance
        // cannot commit the same prepared delta under the same ordinal.
        let mut tails = self.provider_tails.lock().await;
        let tail = tails
            .get_mut(&request.doc_id)
            .context("provider attempt is not active")?;
        let turn = tail.turn;
        let attempt = tail.attempt;
        let capture_scope = tail
            .capture_scope
            .clone()
            .context("provider attempt omitted capture scope")?;
        let ordinal = tail.next_ordinal;
        let delta = provider_flush_delta(&tail.streams, &encoded)?;
        if !delta.grew {
            return Ok(false);
        }
        let segment = OutputSegment {
            agent_did: request.agent_did.clone(),
            requester_did: request.requester_did.clone(),
            session_id: request.session_id.clone(),
            request_doc_id: request.doc_id.clone(),
            source: OutputSource::ProviderTurn {
                scope: capture_scope,
                turn_index: u32::try_from(turn)?,
                attempt,
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: lifecycle.execution_generation()?.to_owned(),
            },
            ordinal: Some(ordinal),
            runs: delta.runs,
            payload: delta.payload,
            close: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        canonical::append_provider_segment(&self.node, lifecycle.execution_generation()?, &segment)
            .await?;
        let tail = tails
            .get_mut(&request.doc_id)
            .context("provider attempt ended during flush")?;
        anyhow::ensure!(
            tail.turn == turn && tail.attempt == attempt && tail.next_ordinal == ordinal,
            "provider attempt changed during flush"
        );
        tail.streams = encoded.streams;
        tail.next_ordinal += 1;
        Ok(true)
    }

    pub(crate) async fn close_provider_attempt(
        &self,
        lifecycle: &crate::lifecycle::RequestLifecycle,
        turn: usize,
        attempt: u32,
        close: canonical::ProviderAttemptClose,
    ) -> Result<()> {
        use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter};
        let request = lifecycle.request();
        // Serialize closing with flushes just as complete publication does.
        // Releasing this lock after reading the scope lets another flush land
        // between choosing the closing timestamp and committing the closure.
        let mut tails = self.provider_tails.lock().await;
        let scope = tails
            .get(&request.doc_id)
            .filter(|tail| tail.turn == turn && tail.attempt == attempt)
            .and_then(|tail| tail.capture_scope)
            .context("provider close has no exact active attempt")?;
        let segment = OutputSegment {
            agent_did: request.agent_did.clone(),
            requester_did: request.requester_did.clone(),
            session_id: request.session_id.clone(),
            request_doc_id: request.doc_id.clone(),
            source: OutputSource::ProviderTurn {
                scope,
                turn_index: u32::try_from(turn)?,
                attempt,
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: lifecycle.execution_generation()?.to_owned(),
            },
            ordinal: None,
            runs: Vec::new(),
            payload: String::new(),
            close: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let partial_message_doc_id = canonical::close_provider_attempt(
            &self.node,
            lifecycle.execution_generation()?,
            &segment,
            close,
        )
        .await?;
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
        lifecycle: &crate::lifecycle::RequestLifecycle,
        turn: usize,
        attempt: u32,
        message: &gents_protocol::message::Message,
    ) -> Result<canonical::PublishedProviderTurn> {
        self.publish_native_turn_with_spawn_admissions(lifecycle, turn, attempt, message, &[])
            .await
    }

    pub(crate) async fn publish_native_turn_with_spawn_admissions(
        &self,
        lifecycle: &crate::lifecycle::RequestLifecycle,
        turn: usize,
        attempt: u32,
        message: &gents_protocol::message::Message,
        spawn_admissions: &[SpawnAdmissionPlan],
    ) -> Result<canonical::PublishedProviderTurn> {
        use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter};

        let request = lifecycle.request();
        let encoded = std::sync::Arc::new(native_encoding::encode_native_message(message)?);
        let expected = std::sync::Arc::new(message.clone());
        // Combine the closure record with the final uncommitted delta when one
        // remains: the one-batch source commits bytes, closure, header and any
        // admissions in one transaction. Replaying a fully flushed turn still
        // closes a terminal-only record.
        let mut tails = self.provider_tails.lock().await;
        // Timestamp only after any in-flight flush has committed. Sampling
        // before this lock can manufacture clock regression during normal
        // concurrent flush/publication, despite a monotonically advancing clock.
        let created_at = chrono::Utc::now().to_rfc3339();
        let tail = tails
            .get_mut(&request.doc_id)
            .filter(|tail| tail.turn == turn && tail.attempt == attempt)
            .context("provider acceptance has no exact active attempt")?;
        let capture_scope = tail
            .capture_scope
            .clone()
            .context("provider acceptance omitted capture scope")?;
        let source = OutputSource::ProviderTurn {
            scope: capture_scope,
            turn_index: u32::try_from(turn).context("provider turn exceeds u32")?,
            attempt,
        };
        let delta = provider_flush_delta(&tail.streams, &encoded)?;
        let segment = if delta.grew {
            OutputSegment {
                agent_did: request.agent_did.clone(),
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
                agent_did: request.agent_did.clone(),
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
                spawn_admissions: spawn_admissions.to_vec(),
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
            .checked_add(self.batch_interval)
            .map(tokio::time::Instant::from_std)
    }

    pub fn new(node: Arc<EmbeddedNode>, _agent_did: &str, batch_interval: Duration) -> Self {
        Self {
            node,
            batch_interval,
            buffers: Mutex::new(HashMap::new()),
            published: Mutex::new(HashMap::new()),
            provider_tails: Mutex::new(HashMap::new()),
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

    pub(crate) async fn discard_buffer(&self, doc_id: &str) {
        self.buffers.lock().await.remove(doc_id);
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
        if !force && !first_visible_content && buf.last_flush_at.elapsed() < self.batch_interval {
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
            && buf.current.reasoning.is_empty()
            && buf.persisted.content_bytes == 0
            && buf.persisted.reasoning.is_empty()
        {
            return Ok(());
        }
        buf.current.content_bytes = 0;
        buf.current.reasoning.clear();
        buf.persisted = buf.current.clone();
        buf.last_flush_at = Instant::now();
        Ok(())
    }
}

impl gents_loop::stream_writer::CanonicalStreamWriter<crate::lifecycle::RequestLifecycle>
    for DefraStreamWriter
{
    type AcceptedToolCall = AcceptedToolCall;
    type SpawnAdmissionPlan = SpawnAdmissionPlan;

    async fn publish_authored_message(
        &self,
        lifecycle: &crate::lifecycle::RequestLifecycle,
        key: &str,
        message: &gents_protocol::message::Message,
    ) -> Result<String> {
        DefraStreamWriter::publish_authored_message(self, lifecycle, key, message).await
    }

    async fn start_provider_attempt(
        &self,
        request_doc_id: &str,
        turn: usize,
        attempt: u32,
        capture_scope: gents_protocol::rendered_request::CaptureScope,
    ) {
        DefraStreamWriter::start_provider_attempt(self, request_doc_id, turn, attempt, capture_scope).await
    }

    async fn flush_native_partial(
        &self,
        lifecycle: &crate::lifecycle::RequestLifecycle,
        message: &gents_protocol::message::Message,
    ) -> Result<bool> {
        DefraStreamWriter::flush_native_partial(self, lifecycle, message).await
    }

    async fn close_provider_attempt(
        &self,
        lifecycle: &crate::lifecycle::RequestLifecycle,
        turn: usize,
        attempt: u32,
        close: gents_loop::stream_writer::ProviderAttemptClose,
    ) -> Result<()> {
        let close = match close {
            gents_loop::stream_writer::ProviderAttemptClose::Retracted => canonical::ProviderAttemptClose::Retracted,
            gents_loop::stream_writer::ProviderAttemptClose::Partial => canonical::ProviderAttemptClose::Partial,
        };
        DefraStreamWriter::close_provider_attempt(self, lifecycle, turn, attempt, close).await
    }

    async fn publish_native_turn_with_spawn_admissions(
        &self,
        lifecycle: &crate::lifecycle::RequestLifecycle,
        turn: usize,
        attempt: u32,
        message: &gents_protocol::message::Message,
        spawn_admissions: &[SpawnAdmissionPlan],
    ) -> Result<gents_loop::stream_writer::CanonicalPublishedTurn<AcceptedToolCall>> {
        let published = DefraStreamWriter::publish_native_turn_with_spawn_admissions(
            self, lifecycle, turn, attempt, message, spawn_admissions,
        ).await?;
        Ok(gents_loop::stream_writer::CanonicalPublishedTurn {
            message_doc_id: published.message_doc_id,
            accepted_tools: published.accepted_tools,
        })
    }
}

impl StreamWriter for DefraStreamWriter {
    async fn write_tokens(&self, doc_id: &str, tokens: &str) -> Result<bool> {
        DefraStreamWriter::write_tokens(self, doc_id, tokens).await
    }

    async fn write_reasoning(&self, doc_id: &str, reasoning: &str) -> Result<bool> {
        DefraStreamWriter::write_reasoning(self, doc_id, reasoning).await
    }

    async fn flush_pending(&self, doc_id: &str) -> Result<bool> {
        DefraStreamWriter::flush_pending(self, doc_id).await
    }

    async fn next_flush_deadline(&self, doc_id: &str) -> Option<tokio::time::Instant> {
        DefraStreamWriter::next_flush_deadline(self, doc_id).await
    }

    async fn reset_tail(&self, doc_id: &str) -> Result<()> {
        DefraStreamWriter::reset_tail(self, doc_id).await
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

/// Prepare the uncommitted per-stream delta against the acknowledged prefix,
/// reusing the flush owner's prefix/declaration and no-shrink validation. The
/// resulting runs/payload fit either an interim flush record or the combined
/// final-flush record of a one-batch source.
struct ProviderStreamDelta {
    runs: Vec<gents_protocol::output::SegmentRun>,
    payload: String,
    /// False when every stream already sits in the acknowledged prefix, so an
    /// interim flush is a no-op and acceptance stays a terminal-only closure.
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

fn tail_window(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut start = value.len() - max_bytes;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_bytes_equal_when_texts_equal() {
        let mut a = StreamBufferSnapshot::default();
        let mut b = StreamBufferSnapshot::default();
        a.record_content("hello ");
        b.record_content("hell");
        b.record_content("o ");
        assert_eq!(a, b, "delta chunking must not change dirty state");
        b.record_content("!");
        assert_ne!(a, b);
    }

    #[test]
    fn first_visible_matches_original_emptiness_checks() {
        let persisted = StreamBufferSnapshot::default();
        let mut current = StreamBufferSnapshot::default();
        assert!(!current.first_visible_since(&persisted));
        current.record_content("hi");
        assert!(current.first_visible_since(&persisted));
        let mut persisted_after_flush = current.clone();
        persisted_after_flush.reasoning.clear();
        persisted_after_flush.reasoning_progress_seq = 0;
        assert!(!current.first_visible_since(&persisted_after_flush));
    }

    #[test]
    fn identical_capped_preview_still_marks_dirty() {
        let mut current = StreamBufferSnapshot::default();
        let oversized = "é".repeat(MAX_LIVE_REASONING_BYTES);
        current.record_reasoning(&oversized);
        let persisted = current.clone();
        assert_eq!(current, persisted, "flush persists the capped preview");
        // A second oversized arrival replaces the preview with identical
        // capped text; the snapshot must still be dirty and first-visible
        // reasoning stays nonempty against the persisted preview.
        current.record_reasoning(&oversized);
        assert_ne!(current, persisted, "progress seq must break the tie");
        assert!(!current.reasoning.is_empty());
        assert_eq!(current.reasoning, persisted.reasoning);
        assert!(current.reasoning.len() <= MAX_LIVE_REASONING_BYTES);
    }

    #[test]
    fn reset_returns_both_epochs_to_empty_content() {
        let mut current = StreamBufferSnapshot::default();
        current.record_content("streamed");
        current.record_reasoning("thought");
        // Mirror reset_tail: current is zeroed, persisted adopts the cleared
        // current, so the next arrival is first-visible again.
        current.content_bytes = 0;
        current.reasoning.clear();
        let persisted = current.clone();
        assert_eq!(current, persisted);
        current.record_content("next turn");
        assert!(current.first_visible_since(&persisted));
        assert_ne!(current, persisted);
    }
}
