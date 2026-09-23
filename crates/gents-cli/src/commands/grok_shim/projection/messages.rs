//! Grok shim message projection.
//!
//! Projects immutable canonical headers and output segments of one request
//! into the Grok pager's streaming `session/update` notification payloads:
//! `agent_message_chunk`, `agent_thought_chunk`, and `user_message_chunk`.
//!
//! The Grok decoder expects the chunk field name
//! `content` (not `contentBlock`): each update payload is
//! `{"sessionUpdate":"agent_message_chunk","content":{"type":"text",
//! "text":"<delta>"}}`. `_meta` is stamped by the projection engine
//! (totalTokens, promptId, isReplay, eventId); this leaf returns the
//! split update shapes and the engine renders the final notification.
//!
//! The projection is bounded and physical-request scoped. Strict reconstruction
//! owns published messages; `output::live` owns pre-publication visibility.
//! Missing dependencies remain loading and conflicts fail closed.
//!
//! All queries go through the in-process embedded node with every
//! interpolated value passed through `escape_graphql_string`; no HTTP
//! GraphQL helper is used.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use gents::graphql::{ensure_no_errors, escape_graphql_string};
use gents_protocol::message::{AssistantContent, Message, UserContent};
use serde_json::{json, Value};

use super::{effective_context_window_tokens, nonempty};

/// `sessionUpdate` discriminators emitted by this leaf.
pub(super) const AGENT_MESSAGE_CHUNK: &str = "agent_message_chunk";
pub(super) const AGENT_THOUGHT_CHUNK: &str = "agent_thought_chunk";
pub(super) const USER_MESSAGE_CHUNK: &str = "user_message_chunk";

/// One projected streaming chunk, split by kind so the projection engine
/// only needs to stamp `_meta` and wrap it in a `session/update`
/// notification.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum MessageUpdate {
    /// Assistant body text delta → `agent_message_chunk`.
    AgentMessageChunk { text: String },
    /// Assistant reasoning delta → `agent_thought_chunk`.
    AgentThoughtChunk { text: String },
    /// Echoed user prompt text → `user_message_chunk`.
    UserMessageChunk { text: String },
}

impl MessageUpdate {
    /// Echo a persisted runtime completion without presenting it as something
    /// the human typed. This is Grok's native ContentChunk metadata; task and
    /// subagent lifecycle projections provide the visible activity instead.
    /// Call only after classifying the durable message key, never from text.
    pub fn background_completion_payload(text: impl Into<String>) -> Value {
        let mut payload = Self::chunk_payload(USER_MESSAGE_CHUNK, text);
        payload["_meta"] = json!({"hideFromScrollback": true});
        payload
    }

    /// The `sessionUpdate` discriminator for this update.
    pub fn session_update_kind(&self) -> &'static str {
        match self {
            MessageUpdate::AgentMessageChunk { .. } => AGENT_MESSAGE_CHUNK,
            MessageUpdate::AgentThoughtChunk { .. } => AGENT_THOUGHT_CHUNK,
            MessageUpdate::UserMessageChunk { .. } => USER_MESSAGE_CHUNK,
        }
    }

    /// Build the chunk payload for one `session_update_kind` discriminator
    /// plus delta text, without constructing an intermediate enum value.
    ///
    /// The live/durable reconciliation in the projection engine emits plain
    /// `(kind, delta)` pairs (a kind string observed from canonical output
    /// or a durable row's chunk kind, plus the byte-exact suffix to send).
    /// `kind` must be one of
    /// [`AGENT_MESSAGE_CHUNK`]/[`AGENT_THOUGHT_CHUNK`]/[`USER_MESSAGE_CHUNK`]; any other value
    /// falls back to `agent_message_chunk` rather than fabricating an
    /// unknown discriminator on the wire.
    pub fn chunk_payload(kind: &str, text: impl Into<String>) -> Value {
        let kind = match kind {
            AGENT_THOUGHT_CHUNK => AGENT_THOUGHT_CHUNK,
            USER_MESSAGE_CHUNK => USER_MESSAGE_CHUNK,
            _ => AGENT_MESSAGE_CHUNK,
        };
        json!({
            "sessionUpdate": kind,
            "content": {
                "type": "text",
                "text": text.into(),
            },
        })
    }
}

/// The full set of streaming message updates for one request id, in transcript
/// order, plus the projection bookkeeping the engine needs to stamp `_meta`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct MessageProjection {
    /// Ordered streaming updates: `user_message_chunk` echoes precede
    /// assistant output, and assistant rows project in `sequence` order.
    pub updates: Vec<MessageUpdate>,
    /// Durable chronology key per update, aligned 1:1 with `updates`: the
    /// row's `sequence` in the shared transcript sequence space (the same
    /// space `AgentToolCall.message_sequence` allocates from). `None` when
    /// the row carries no sequence — such updates sort after every
    /// positioned event of the family.
    pub chronology: Vec<Option<i64>>,
    /// Durable chunk identity per update, aligned 1:1 with `updates`:
    /// `"{message_key}:{update kind}:{per-row ordinal of that kind}"`. The
    /// live projection poll deduplicates streamed chunks by these keys, so
    /// two distinct rows carrying identical text both stream *and* one row's
    /// reasoning thought and body text are distinct chunks. An entry is
    /// empty only if the aligned update's row could not be identified (never
    /// happens today: every update comes from a decoded row).
    pub update_keys: Vec<String>,
    /// Generated-token accounting is not part of canonical output.
    pub total_tokens: u64,
    /// Whether the projected request is terminal (complete, error, or
    /// non-empty `interrupted_at`). A still-running request is not terminal
    /// and the engine keeps the pending prompt unresolved.
    pub terminal: bool,
    /// Terminal stop reason projection when `terminal` is true. This is an
    /// adapter projection, not a persisted field: `cancelled` for an
    /// interrupted turn, `error` for a failed one, `end_turn` otherwise.
    pub stop_reason: Option<&'static str>,
    /// Context window tokens used to bound `totalTokens`; falls back to the
    /// catalog default when the bound configuration did not supply one.
    pub context_window_tokens: u64,
    /// Canonical live projection for the currently eligible source.
    pub live_tail: LiveResponseTail,
    /// Exact canonical source-to-header bindings derived from payload close
    /// document identities. Text equality is never used as provenance.
    pub canonical_bindings: Vec<CanonicalSourceBinding>,
    /// Inclusive durable transcript high-water proved by this projection.
    /// The caller commits it only after every event in the batch sends.
    pub message_sequence_high_water: Option<i64>,
    /// Durable request start used as the first generation timing candidate.
    pub response_started_at_ms: Option<i64>,
    /// Durable terminal timestamp bounds the retained streaming tail during
    /// replay. Arrival time is not historical generation time.
    pub response_ended_at_ms: Option<i64>,
    /// Request-local transcript timestamps observed by this bounded read.
    /// Projection retains these across polls to derive the start of later
    /// tool-loop generations from the preceding durable input row.
    pub timeline: Vec<MessageTimelineRow>,
}

/// Timestamp-bearing identity of one request-local transcript row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MessageTimelineRow {
    pub sequence: i64,
    pub message_key: String,
    pub timestamp_ms: Option<i64>,
}

/// One protocol-owned canonical live observation.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct LiveResponseTail {
    /// Live content prefix, verbatim.
    pub content: Option<String>,
    /// Live reasoning prefix, verbatim.
    pub reasoning: Option<String>,
    /// Sequence of the current assistant `AgentMessage` row (the row the
    /// live tail belongs to), when one exists. Used as the live segment's
    /// chronology position.
    pub assistant_sequence: Option<i64>,
    pub source_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CanonicalSourceBinding {
    pub source_key: String,
    pub sequence: i64,
    pub message_key: String,
}

/// The query execution seam this leaf reads through.
///
/// Production always executes through the embedded node. The seam exists so
/// tests can supply bounded canonical header/segment facts. `QuerySink` is
/// internal to this leaf (the public [`project_messages`] entry point keeps the
/// `Arc<EmbeddedNode>` signature); the shared `execute` helper below keeps
/// every read on one seam so ordering regressions cannot hide behind a
/// direct `node.execute` call. The returned future is `Send` so the
/// loader's futures stay `Send` end to end without a proc-macro crate.
pub(super) trait QuerySink: Send + Sync {
    fn execute(
        &self,
        query: &str,
    ) -> impl std::future::Future<Output = defra_node::QueryResponse> + Send;
}

/// The production sink: the embedded node itself.
struct NodeSink<'a> {
    node: &'a Arc<EmbeddedNode>,
}

impl QuerySink for NodeSink<'_> {
    async fn execute(&self, query: &str) -> defra_node::QueryResponse {
        self.node.execute(query).await
    }
}

/// Project the streaming message chunks for one request id.
///
/// Bounded and request-id-scoped: the query set is exactly
/// The single query reads bounded immutable headers and segment facts.
///
/// It never replays the session, never duplicates durable materialization
/// (the projection is read-only), and every payload is a fresh notification
/// value. Returns an empty projection when the request has no rows.
pub(super) async fn project_messages(
    node: &Arc<EmbeddedNode>,
    message_sequence_high_water: Option<i64>,
    request: &gents_protocol::row::AgentRequestRow,
    context_window_tokens: u64,
) -> Result<MessageProjection> {
    let sink = NodeSink { node };
    project_messages_with_sink(
        &sink,
        message_sequence_high_water,
        request,
        context_window_tokens,
    )
    .await
}

/// The loader body on one query sink; see [`project_messages`] for the
/// contract and [`QuerySink`] for why the seam is separated out.
async fn project_messages_with_sink<S: QuerySink>(
    sink: &S,
    message_sequence_high_water: Option<i64>,
    request: &gents_protocol::row::AgentRequestRow,
    context_window_tokens: u64,
) -> Result<MessageProjection> {
    use gents::session::canonical_rows::{
        decode_output_segment_row, decode_transcript_message_row, AGENT_MESSAGE_FIELDS,
        AGENT_OUTPUT_SEGMENT_FIELDS,
    };
    use gents_protocol::output::live::{
        observed_request_execution_owner, project_live, select_live_target, LiveObservation,
        LiveTarget, LiveTargetSelection, LiveView, OwnerLiveness,
    };
    use gents_protocol::output::reconstruction::{reconstruct_message, ObservedSegment};
    use gents_protocol::output::{MessageRole, StreamPayload};
    let physical = request
        .doc_id
        .as_deref()
        .and_then(nonempty)
        .ok_or_else(|| anyhow!("message request physical identity missing"))?;
    let owner = request
        .agent_did
        .as_deref()
        .and_then(nonempty)
        .ok_or_else(|| anyhow!("message request principal missing"))?;
    let session = request
        .session_id
        .as_deref()
        .and_then(nonempty)
        .ok_or_else(|| anyhow!("message request session missing"))?;
    let scope =
        gents::session::session_scope_filter(owner, session, request.requester_did.as_deref());
    let query = format!(
        r#"{{
        AgentMessage(filter:{{{scope},request_doc_id:{{_eq:"{}"}}}},order:{{sequence:ASC}},limit:256){{{AGENT_MESSAGE_FIELDS}}}
        AgentOutputSegment(filter:{{{scope},request_doc_id:{{_eq:"{}"}}}},limit:256){{{AGENT_OUTPUT_SEGMENT_FIELDS}}}
    }}"#,
        escape_graphql_string(physical),
        escape_graphql_string(physical)
    );
    let response = sink.execute(&query).await;
    ensure_no_errors(&response, "grok canonical message projection")?;
    let data = response
        .data
        .as_ref()
        .context("canonical message projection omitted data")?;
    let header_values = data
        .get("AgentMessage")
        .and_then(Value::as_array)
        .context("canonical projection omitted headers")?;
    let segment_values = data
        .get("AgentOutputSegment")
        .and_then(Value::as_array)
        .context("canonical projection omitted segments")?;
    anyhow::ensure!(
        header_values.len() < 256 && segment_values.len() < 256,
        "canonical Grok projection exceeded bounded page"
    );
    let headers = header_values
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    let segments = segment_values
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()?;
    let observed = segments
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let mut updates = Vec::new();
    let mut update_keys = Vec::new();
    let mut chronology = Vec::new();
    let floor = message_sequence_high_water.unwrap_or(-1);
    let mut high_water = message_sequence_high_water;
    let mut timeline = Vec::new();
    let messages = headers
        .iter()
        .map(|row| (row.doc_id.as_str(), &row.message))
        .collect::<Vec<_>>();
    for row in &headers {
        let sequence = i64::from(row.message.sequence);
        high_water = Some(high_water.map_or(sequence, |prior| prior.max(sequence)));
        timeline.push(MessageTimelineRow {
            sequence,
            message_key: row.message.message_key.clone(),
            timestamp_ms: rfc3339_millis(&row.message.created_at),
        });
        if sequence <= floor {
            continue;
        }
        let native = match reconstruct_message(&observed, &[], &[], &row.message) {
            Ok(message) => message,
            Err(error) if error.is_incomplete() => continue,
            Err(error) => {
                return Err(anyhow!(error).context("reconstructing canonical Grok message"));
            }
        };
        let before = updates.len();
        project_native_message(&native, &mut updates);
        let mut kinds_seen: std::collections::BTreeMap<&'static str, u64> =
            std::collections::BTreeMap::new();
        for update in &updates[before..] {
            let ordinal = {
                let counter = kinds_seen.entry(update.session_update_kind()).or_default();
                *counter += 1;
                *counter
            };
            update_keys.push(format!(
                "{}:{}:{}",
                row.message.message_key,
                update.session_update_kind(),
                ordinal
            ));
            chronology.push(Some(sequence));
        }
    }
    let mut live_tail = LiveResponseTail::default();
    let mut canonical_bindings = Vec::new();
    for header in &headers {
        for reference in header.message.payload_references() {
            if let Some(segment) = segments
                .iter()
                .find(|row| row.doc_id == reference.close_doc_id)
            {
                let source_key = serde_json::to_string(&segment.segment.source)?;
                if !canonical_bindings
                    .iter()
                    .any(|binding: &CanonicalSourceBinding| {
                        binding.source_key == source_key
                            && binding.sequence == i64::from(header.message.sequence)
                            && binding.message_key == header.message.message_key
                    })
                {
                    canonical_bindings.push(CanonicalSourceBinding {
                        source_key,
                        sequence: i64::from(header.message.sequence),
                        message_key: header.message.message_key.clone(),
                    });
                }
            }
        }
    }
    let selection = request
        .execution_generation
        .as_deref()
        .map_or(LiveTargetSelection::Absent, |generation| {
            select_live_target(physical, generation, &observed, &messages)
        });
    if matches!(selection, LiveTargetSelection::Conflicted) {
        return Err(anyhow!("canonical Grok live target conflicted"));
    }
    if let LiveTargetSelection::Selected {
        source,
        writer,
        message_id,
    } = selection
    {
        let source_key = serde_json::to_string(&source)?;
        let view = project_live(&LiveObservation {
            request_doc_id: physical,
            session_id: session,
            target: LiveTarget {
                request_doc_id: physical,
                source: &source,
                writer: &writer,
                message_id: message_id.as_deref(),
            },
            messages: &messages,
            agent_did: owner,
            requester_did: request.requester_did.as_deref(),
            records: &observed,
            denied_headers: &[],
            denied_segments: &[],
            dependency_denials: &[],
            owner: OwnerLiveness {
                current_request: observed_request_execution_owner(request),
                live_tools: Vec::new(),
            },
            request_terminal: request
                .lifecycle_state
                .is_some_and(gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal),
            terminal_selection: request.terminal_output.clone(),
        });
        let streams = match view {
            LiveView::Live { streams } | LiveView::Settling { streams } => streams,
            LiveView::Absent
            | LiveView::Loading
            | LiveView::Retracted
            | LiveView::Published { .. }
            | LiveView::RetainedPartial { .. } => Vec::new(),
            LiveView::Denied => return Err(anyhow!("canonical Grok output denied")),
            LiveView::Conflicted => return Err(anyhow!("canonical Grok output conflicted")),
            LiveView::Invalid => return Err(anyhow!("canonical Grok output invalid")),
        };
        if !streams.is_empty() {
            live_tail.source_key = Some(source_key);
        }
        for stream in streams {
            match stream.declaration.payload {
                StreamPayload::Text => live_tail
                    .content
                    .get_or_insert_with(String::new)
                    .push_str(&stream.text),
                StreamPayload::Reasoning | StreamPayload::ReasoningSummary => live_tail
                    .reasoning
                    .get_or_insert_with(String::new)
                    .push_str(&stream.text),
                _ => {}
            }
        }
    }
    live_tail.assistant_sequence = headers
        .iter()
        .rev()
        .find(|row| row.message.role == MessageRole::Assistant)
        .map(|row| i64::from(row.message.sequence));
    let terminal = request
        .lifecycle_state
        .is_some_and(gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal);
    let stop_reason = terminal.then_some(match request.lifecycle_state {
        Some(
            gents_protocol::request_lifecycle::RequestLifecycleState::Failed
            | gents_protocol::request_lifecycle::RequestLifecycleState::Dead,
        ) => "error",
        Some(gents_protocol::request_lifecycle::RequestLifecycleState::Interrupted) => "cancelled",
        _ => "end_turn",
    });
    Ok(MessageProjection {
        updates,
        update_keys,
        chronology,
        total_tokens: 0,
        terminal,
        stop_reason,
        context_window_tokens: effective_context_window_tokens(context_window_tokens),
        live_tail,
        canonical_bindings,
        message_sequence_high_water: high_water,
        response_started_at_ms: request.created_at.as_deref().and_then(rfc3339_millis),
        response_ended_at_ms: request.terminalized_at.as_deref().and_then(rfc3339_millis),
        timeline,
    })
}

fn project_native_message(message: &Message, updates: &mut Vec<MessageUpdate>) {
    match message {
        Message::Assistant { content, .. } => {
            for item in content {
                if let AssistantContent::Reasoning(reasoning) = item {
                    for text in reasoning_texts(reasoning) {
                        push_nonempty(updates, MessageUpdate::AgentThoughtChunk { text });
                    }
                }
            }
            for item in content {
                if let AssistantContent::Text(text) = item {
                    if let Some(text) = streamable_owned(&text.text) {
                        updates.push(MessageUpdate::AgentMessageChunk { text });
                    }
                }
            }
        }
        Message::User { content } => {
            for item in content {
                if let UserContent::Text(text) = item {
                    if let Some(text) = streamable_owned(&text.text) {
                        updates.push(MessageUpdate::UserMessageChunk { text });
                    }
                }
            }
        }
        Message::System { .. } => {}
    }
}

fn rfc3339_millis(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

fn push_nonempty(updates: &mut Vec<MessageUpdate>, update: MessageUpdate) {
    let is_empty = match &update {
        MessageUpdate::AgentMessageChunk { text }
        | MessageUpdate::AgentThoughtChunk { text }
        | MessageUpdate::UserMessageChunk { text } => text.trim().is_empty(),
    };
    if !is_empty {
        updates.push(update);
    }
}

/// Text pieces of a reasoning block, rendered the way the transcript
/// presents them (plain text and summary text stream; encrypted/redacted
/// payloads are opaque and never stream as thought text).
fn reasoning_texts(reasoning: &gents_protocol::message::Reasoning) -> Vec<String> {
    use gents_protocol::message::ReasoningContent;
    reasoning
        .content
        .iter()
        .filter_map(|item| match item {
            ReasoningContent::Text { text, .. } | ReasoningContent::Summary(text) => {
                streamable_owned(text)
            }
            ReasoningContent::Encrypted(_) | ReasoningContent::Redacted { .. } => None,
        })
        .collect()
}

/// Streamable chunk text: verbatim (never trimmed) but whitespace-only
/// blocks are skipped so a blank block does not emit an empty chunk.
fn streamable_owned(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod canonical_selection_tests {
    use super::*;
    use gents_protocol::request_lifecycle::RequestLifecycleState;

    struct Facts {
        headers: Vec<Value>,
        segments: Vec<Value>,
    }

    impl QuerySink for Facts {
        async fn execute(&self, _query: &str) -> defra_node::QueryResponse {
            defra_node::QueryResponse::success(json!({
                "AgentMessage": self.headers,
                "AgentOutputSegment": self.segments,
            }))
        }
    }

    fn request() -> gents_protocol::row::AgentRequestRow {
        gents_protocol::row::AgentRequestRow {
            doc_id: Some("request-doc".into()),
            request_id: "request-1".into(),
            agent_did: Some("did:test:grok".into()),
            session_id: Some("session-1".into()),
            lifecycle_state: Some(RequestLifecycleState::Processing),
            execution_generation: Some("generation-1".into()),
            execution_lease_secs: Some(30),
            execution_lease_expires_at: Some("2026-09-22T00:01:00Z".into()),
            created_at: Some("2026-09-22T00:00:00Z".into()),
            ..Default::default()
        }
    }

    fn text_segment(doc_id: &str, turn: u32, text: &str, outcome: Option<&str>) -> Value {
        let mut segment = json!({
            "_docID": doc_id,
            "agent_did": "did:test:grok",
            "session_id": "session-1",
            "request_doc_id": "request-doc",
            "source": {"kind":"provider_turn","scope":"inference.0","turn_index":turn,"attempt":0},
            "writer": {"kind":"request_execution","execution_generation":"generation-1"},
            "ordinal": 0,
            "runs": [{"stream":0,"bytes":text.len(),"declaration":{
                "block_index":0,"part_index":0,"payload":{"kind":"text"}
            }}],
            "payload": text,
            "created_at": "2026-09-22T00:00:01Z"
        });
        if let Some(outcome) = outcome {
            segment["close"] = json!({
                "kind":"closed","outcome":outcome,"segments":1,"stream_bytes":[text.len()]
            });
        }
        segment
    }

    fn assistant_header(doc_id: &str, key: &str, close_doc_id: &str) -> Value {
        json!({
            "_docID": doc_id,
            "message_key": key,
            "session_id": "session-1",
            "agent_did": "did:test:grok",
            "request_doc_id": "request-doc",
            "publication": {"kind":"request_execution","execution_generation":"generation-1"},
            "outcome": "complete",
            "sequence": 1,
            "role": "assistant",
            "blocks": [{"type":"text","text":{
                "output":{"close_doc_id":close_doc_id,"stream":0},
                "presentation":{"kind":"full"}
            }}],
            "created_at": "2026-09-22T00:00:02Z"
        })
    }

    async fn project(facts: &Facts) -> Result<MessageProjection> {
        project_messages_with_sink(facts, None, &request(), 8192).await
    }

    async fn project_with_request(
        facts: &Facts,
        request: &gents_protocol::row::AgentRequestRow,
    ) -> Result<MessageProjection> {
        project_messages_with_sink(facts, None, request, 8192).await
    }

    #[tokio::test]
    async fn latest_provider_source_wins_over_older_partial_and_published_history() {
        let facts = Facts {
            headers: vec![assistant_header("header-1", "old-published", "old-close")],
            segments: vec![
                text_segment("old-close", 0, "published old", Some("complete")),
                text_segment("partial-close", 1, "older partial", Some("partial")),
                text_segment("current-flush", 2, "current live", None),
            ],
        };
        let projection = project(&facts).await.expect("canonical selection");
        assert!(projection.updates.iter().any(|update| matches!(update,
            MessageUpdate::AgentMessageChunk { text } if text == "published old")));
        assert_eq!(
            projection.live_tail.content.as_deref(),
            Some("current live")
        );
        assert!(!projection
            .live_tail
            .content
            .as_deref()
            .unwrap()
            .contains("older"));
    }

    #[tokio::test]
    async fn active_partial_settles_but_terminal_retained_partial_is_not_live() {
        let partial = Facts {
            headers: vec![],
            segments: vec![text_segment(
                "partial-close",
                0,
                "retained",
                Some("partial"),
            )],
        };
        assert_eq!(
            project(&partial)
                .await
                .unwrap()
                .live_tail
                .content
                .as_deref(),
            Some("retained"),
            "a current producer's closed Partial settles before terminal selection"
        );
        let mut terminal_request = request();
        terminal_request.lifecycle_state = Some(RequestLifecycleState::Failed);
        terminal_request.terminal_output = Some(gents_protocol::output::TerminalOutput::NoMessage);
        assert_eq!(
            project_with_request(&partial, &terminal_request)
                .await
                .unwrap()
                .live_tail
                .content,
            None,
            "terminal unreferenced Partial is retained diagnostic, not live output"
        );
        let published = Facts {
            headers: vec![assistant_header("header-1", "published", "complete-close")],
            segments: vec![text_segment(
                "complete-close",
                0,
                "already durable",
                Some("complete"),
            )],
        };
        assert_eq!(project(&published).await.unwrap().live_tail.content, None);
    }

    #[tokio::test]
    async fn conflicting_current_source_fails_closed() {
        let facts = Facts {
            headers: vec![],
            segments: vec![
                text_segment("flush-a", 0, "first", None),
                text_segment("flush-b", 0, "other", None),
            ],
        };
        assert!(project(&facts).await.is_err());
    }
}
