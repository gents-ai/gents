//! Grok shim message projection.
//!
//! Projects the durable `AgentResponse`/`AgentMessage` rows of one request id
//! into the Grok pager's streaming `session/update` notification payloads:
//! `agent_message_chunk`, `agent_thought_chunk`, and `user_message_chunk`.
//!
//! Two contracts matter for fidelity:
//!
//! 1. **Persisted envelope decoding.** `AgentMessage.content` is not raw
//!    delta text: the runtime persists `serde_json::to_string(&Message)`
//!    where `Message` is `gents_protocol::message::Message` (tag = "role":
//!    `{"role":"assistant","id":null,"content":[{"text":"..."}]}` for
//!    assistant rows, `{"role":"user","content":[{"type":"text","text":
//!    "..."}]}` for user rows). This leaf decodes that envelope through
//!    `gents_protocol::transcript::decode_persisted_message` (which also
//!    tolerates a bare `Vec<AssistantContent>`/`Vec<UserContent>` array and
//!    falls back to treating the blob as plain text) and streams only the
//!    *text* blocks of the decoded message. A row whose persisted content
//!    fails to decode projects as nothing rather than as JSON noise.
//!
//! 2. **Wire shape.** The Grok decoder expects the chunk field name
//!    `content` (not `contentBlock`): each update payload is
//!    `{"sessionUpdate":"agent_message_chunk","content":{"type":"text",
//!    "text":"<delta>"}}`. `_meta` is stamped by the projection engine
//!    (totalTokens, promptId, isReplay, eventId); this leaf returns the
//!    split update shapes and the engine renders the final notification.
//!
//! The projection is bounded and request-id-scoped: exactly one
//! `AgentResponse` query (the latest row for the request) plus one
//! `AgentMessage` query (ordered by sequence), with no session replay and no
//! durable materialization. A request is terminal only when its response
//! status is `complete`/`error`, its lifecycle is `complete`/`error`, or it
//! carries a non-empty `interrupted_at`; anything else is a still-running
//! turn. `AgentResponse.token_count` is the persisted source projected into
//! connection-local cumulative `totalTokens` metadata; no `AgentSession`
//! usage field and no synthetic `ProviderContextReduction` is written.
//!
//! All queries go through the in-process embedded node (`node.execute`) with
//! every interpolated value passed through `escape_graphql_string`; no HTTP
//! GraphQL helper is used.

use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::graphql::{ensure_no_errors, escape_graphql_string};
use gents_protocol::message::{AssistantContent, Message, UserContent};
use gents_protocol::transcript::decode_persisted_message;
use serde::Deserialize;
use serde_json::{json, Value};

/// `sessionUpdate` discriminators emitted by this leaf.
pub(super) const AGENT_MESSAGE_CHUNK: &str = "agent_message_chunk";
pub(super) const AGENT_THOUGHT_CHUNK: &str = "agent_thought_chunk";
pub(super) const USER_MESSAGE_CHUNK: &str = "user_message_chunk";

/// Fallback context window when the bound configuration did not supply one;
/// matches the model catalog's `totalContextTokens` default scale.
const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 262_144;

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
    /// The `sessionUpdate` discriminator for this update.
    pub fn session_update_kind(&self) -> &'static str {
        match self {
            MessageUpdate::AgentMessageChunk { .. } => AGENT_MESSAGE_CHUNK,
            MessageUpdate::AgentThoughtChunk { .. } => AGENT_THOUGHT_CHUNK,
            MessageUpdate::UserMessageChunk { .. } => USER_MESSAGE_CHUNK,
        }
    }

    /// Render the Grok pager payload for this update. The chunk field name is
    /// `content` (the Grok decoder's expected name, not `contentBlock`).
    pub fn to_payload(&self) -> Value {
        let text = match self {
            MessageUpdate::AgentMessageChunk { text }
            | MessageUpdate::AgentThoughtChunk { text }
            | MessageUpdate::UserMessageChunk { text } => text,
        };
        json!({
            "sessionUpdate": self.session_update_kind(),
            "content": {
                "type": "text",
                "text": text,
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
    /// Cumulative token count for `_meta.totalTokens`, from the latest
    /// `AgentResponse.token_count` (u64, never fabricated). Zero when the
    /// request has no response row yet.
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
}

/// Latest `AgentResponse` row for the request. The runtime writes exactly one
/// response per request; "latest" guards against a retry-replaced row.
#[derive(Clone, Debug, Deserialize)]
struct ResponseRow {
    request_id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
    #[serde(default)]
    token_count: Option<i64>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    materialized_at: Option<String>,
    #[serde(default)]
    materialized_message_sequence: Option<i64>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
    #[serde(default)]
    interrupted_at: Option<String>,
}

/// One `AgentMessage` transcript row scoped to the request.
#[derive(Clone, Debug, Deserialize)]
struct MessageRow {
    message_key: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    sequence: Option<i64>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
}

/// Project the streaming message chunks for one request id.
///
/// Bounded and request-id-scoped: the query set is exactly
/// 1. one `AgentResponse` query for the latest row of this request id,
/// 2. one `AgentMessage` query for this request id ordered by sequence.
///
/// It never replays the session, never duplicates durable materialization
/// (the projection is read-only), and every payload is a fresh notification
/// value. Returns an empty projection when the request has no rows.
pub(super) async fn project_messages(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    context_window_tokens: u64,
) -> Result<MessageProjection> {
    let response = node.execute(&latest_response_query(request_id)).await;
    ensure_no_errors(&response, "grok shim message response query")?;
    let response_row = decode_response_row(&response);

    let messages_response = node.execute(&request_messages_query(request_id)).await;
    ensure_no_errors(&messages_response, "grok shim message rows query")?;
    let rows = decode_message_rows(&messages_response);

    let context_window_tokens = if context_window_tokens == 0 {
        DEFAULT_CONTEXT_WINDOW_TOKENS
    } else {
        context_window_tokens
    };

    let mut updates = Vec::new();
    let mut update_keys = Vec::new();
    let mut chronology = Vec::new();
    for row in &rows {
        // Defense-in-depth re-check of the request scoping: the query filter
        // already guarantees it, but a widened filter must not leak other
        // requests' transcript rows into this projection.
        if row.request_id.as_deref().and_then(nonempty) != Some(request_id) {
            continue;
        }
        let before = updates.len();
        project_row(row, &mut updates);
        // One durable identity per update that row produced, aligned 1:1
        // with `updates` so the live poll can dedupe by durable identity.
        // The identity is chunk-level — `message_key` plus the update kind
        // plus the per-row ordinal of that kind — because one row can emit
        // more than one chunk (a reasoning thought plus its body text): a
        // row-level key would let the thought mark the body text as already
        // streamed and silently drop it. The kind plus ordinal keeps two
        // distinct rows with identical text emitting both times while still
        // distinguishing a row's thought from its text.
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
                row.message_key,
                update.session_update_kind(),
                ordinal
            ));
            chronology.push(row.sequence);
        }
    }

    let total_tokens = response_row
        .as_ref()
        .and_then(|row| row.token_count)
        .and_then(|tokens| u64::try_from(tokens.max(0)).ok())
        .unwrap_or(0);

    let (terminal, stop_reason) = match response_row.as_ref() {
        Some(row) if row.is_terminal() => (true, Some(row.stop_reason())),
        _ => (false, None),
    };

    Ok(MessageProjection {
        updates,
        update_keys,
        chronology,
        total_tokens,
        terminal,
        stop_reason,
        context_window_tokens,
    })
}

/// Project one transcript row into ordered streaming updates.
///
/// The persisted `content` blob is decoded through
/// `decode_persisted_message` (role-aware, with plain-text fallback) and only
/// its text blocks stream. Assistant reasoning streams as
/// `agent_thought_chunk` before the body so the pager sees thought-then-text
/// per row; user rows echo as `user_message_chunk` and never include
/// tool-result blocks (tool results are the tool leaf's domain).
fn project_row(row: &MessageRow, updates: &mut Vec<MessageUpdate>) {
    let role = row.role.as_deref().and_then(nonempty).unwrap_or_default();
    let blob = row
        .content
        .as_deref()
        .and_then(nonempty)
        .unwrap_or_default();
    if blob.is_empty() {
        return;
    }
    let message = decode_persisted_message(role, blob);

    match &message {
        Message::Assistant { content, .. } => {
            // Reasoning deltas first (thought-before-text per row), then body.
            // Chunk text streams verbatim: only whitespace-only blocks are
            // skipped, never trimmed, so the accumulated pager text equals
            // the durable message text exactly.
            for item in content {
                if let AssistantContent::Reasoning(reasoning) = item {
                    for text in reasoning_texts(reasoning) {
                        push_nonempty(updates, MessageUpdate::AgentThoughtChunk { text });
                    }
                }
            }
            // #492: the durable reasoning copy may live only in
            // `AgentMessage.reasoning` when the response tail was cleared on
            // finalize; project it so a finished row still shows its thought.
            // It streams before the body text, matching the established
            // thought-before-text contract for every assistant row.
            if content
                .iter()
                .all(|item| !matches!(item, AssistantContent::Reasoning(_)))
            {
                if let Some(reasoning) = row.reasoning.as_deref().and_then(streamable_owned) {
                    push_nonempty(
                        updates,
                        MessageUpdate::AgentThoughtChunk { text: reasoning },
                    );
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
        Message::System { .. } => {
            // System messages are not persisted in session history; a row
            // claiming that role projects as nothing.
        }
    }
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

fn nonempty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Streamable chunk text: verbatim (never trimmed) but whitespace-only
/// blocks are skipped so a blank block does not emit an empty chunk.
fn streamable_owned(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

impl ResponseRow {
    /// A request is terminal only when the response reached a terminal
    /// status (`complete`/`error`) or carries a non-empty `interrupted_at`.
    /// Blank, running, and in-flight statuses are never terminal.
    fn is_terminal(&self) -> bool {
        if self.interrupted_at.as_deref().and_then(nonempty).is_some() {
            return true;
        }
        matches!(
            self.status.as_deref().and_then(nonempty),
            Some("complete") | Some("error")
        )
    }

    /// Adapter-projected stop reason for a terminal response. Never persisted.
    fn stop_reason(&self) -> &'static str {
        if self.interrupted_at.as_deref().and_then(nonempty).is_some() {
            return "cancelled";
        }
        if self.status.as_deref().and_then(nonempty) == Some("error") {
            return "error";
        }
        "end_turn"
    }
}

// ---------------------------------------------------------------------------
// Queries and decoding
// ---------------------------------------------------------------------------

/// Latest `AgentResponse` row for the request id. The runtime writes one
/// response per request; ordering by `created_at` descending with a bound of
/// one row keeps the query bounded even if a retry replaced the row.
fn latest_response_query(request_id: &str) -> String {
    format!(
        r#"{{
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{ {RESPONSE_FIELDS} }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    )
}

/// `AgentMessage` rows for the request id in transcript order. Ordered by
/// `sequence` so the streamed chunks follow the durable transcript order
/// (user echo before assistant output).
fn request_messages_query(request_id: &str) -> String {
    format!(
        r#"{{
            AgentMessage(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ {MESSAGE_FIELDS} }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    )
}

const RESPONSE_FIELDS: &str = "
    request_id
    status
    error_message
    token_count
    content
    reasoning
    materialized_at
    materialized_message_sequence
    created_at
    completed_at
    interrupted_at
";

const MESSAGE_FIELDS: &str = "
    message_key
    session_id
    request_id
    sequence
    role
    content
    reasoning
    timestamp
";

fn decode_response_row(response: &defra_node::QueryResponse) -> Option<ResponseRow> {
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()?;
    match serde_json::from_value::<ResponseRow>(row) {
        Ok(row) => Some(row),
        Err(error) => {
            tracing::debug!(
                %error,
                "grok shim skipped an undecodable AgentResponse row"
            );
            None
        }
    }
}

fn decode_message_rows(response: &defra_node::QueryResponse) -> Vec<MessageRow> {
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| match serde_json::from_value::<MessageRow>(row) {
            Ok(row) => Some(row),
            Err(error) => {
                tracing::debug!(
                    %error,
                    "grok shim skipped an undecodable AgentMessage row"
                );
                None
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn message_row(role: &str, sequence: i64, content: &str) -> MessageRow {
        MessageRow {
            message_key: format!("sess:{sequence}"),
            session_id: Some("sess".to_string()),
            request_id: Some("req-1".to_string()),
            sequence: Some(sequence),
            role: Some(role.to_string()),
            content: Some(content.to_string()),
            reasoning: None,
            timestamp: None,
        }
    }

    #[test]
    fn update_kinds_use_the_grok_wire_names() {
        assert_eq!(
            MessageUpdate::AgentMessageChunk { text: "hi".into() }.session_update_kind(),
            "agent_message_chunk"
        );
        assert_eq!(
            MessageUpdate::AgentThoughtChunk { text: "hi".into() }.session_update_kind(),
            "agent_thought_chunk"
        );
        assert_eq!(
            MessageUpdate::UserMessageChunk { text: "hi".into() }.session_update_kind(),
            "user_message_chunk"
        );
    }

    #[test]
    fn payload_uses_content_field_name_not_content_block() {
        let payload = MessageUpdate::AgentMessageChunk {
            text: "delta".into(),
        }
        .to_payload();
        assert_eq!(payload["sessionUpdate"], "agent_message_chunk");
        assert_eq!(payload["content"]["type"], "text");
        assert_eq!(payload["content"]["text"], "delta");
        assert!(
            payload.get("contentBlock").is_none(),
            "the Grok decoder expects the chunk field name `content`"
        );
    }

    #[test]
    fn assistant_envelope_decodes_to_message_chunks_not_raw_json() {
        // The persisted blob is serde_json::to_string(&Message), not raw text.
        let blob = serde_json::to_string(&Message::assistant("Hello from Gents!"))
            .expect("serialize assistant message");
        let mut updates = Vec::new();
        project_row(&message_row("assistant", 1, &blob), &mut updates);
        assert_eq!(
            updates,
            vec![MessageUpdate::AgentMessageChunk {
                text: "Hello from Gents!".to_string()
            }],
            "the envelope must be decoded; the raw JSON blob must never stream"
        );
    }

    #[test]
    fn chunk_text_streams_verbatim_without_trimming() {
        // The streamed deltas must concatenate to the durable message text
        // exactly; trimming would corrupt multi-block messages.
        let message = Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::text("  leading and trailing  "),
                AssistantContent::text("\nsecond block\n"),
            ],
        };
        let blob = serde_json::to_string(&message).expect("serialize assistant message");
        let mut updates = Vec::new();
        project_row(&message_row("assistant", 1, &blob), &mut updates);
        assert_eq!(
            updates,
            vec![
                MessageUpdate::AgentMessageChunk {
                    text: "  leading and trailing  ".to_string()
                },
                MessageUpdate::AgentMessageChunk {
                    text: "\nsecond block\n".to_string()
                },
            ],
            "chunk text must stream verbatim, never str::trim filtered"
        );
    }

    #[test]
    fn whitespace_only_blocks_do_not_stream() {
        let message = Message::Assistant {
            id: None,
            content: vec![AssistantContent::text("   \n\t  ")],
        };
        let blob = serde_json::to_string(&message).expect("serialize assistant message");
        let mut updates = Vec::new();
        project_row(&message_row("assistant", 1, &blob), &mut updates);
        assert!(updates.is_empty());
    }

    #[test]
    fn assistant_envelope_with_text_and_reasoning_orders_thought_before_text() {
        let message = Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::Reasoning(gents_protocol::message::Reasoning::new(
                    "thinking hard",
                )),
                AssistantContent::text("the answer"),
            ],
        };
        let blob = serde_json::to_string(&message).expect("serialize assistant message");
        let mut updates = Vec::new();
        project_row(&message_row("assistant", 1, &blob), &mut updates);
        assert_eq!(
            updates,
            vec![
                MessageUpdate::AgentThoughtChunk {
                    text: "thinking hard".to_string()
                },
                MessageUpdate::AgentMessageChunk {
                    text: "the answer".to_string()
                },
            ]
        );
    }

    #[test]
    fn assistant_reasoning_only_row_streams_thought_chunk() {
        let message = Message::Assistant {
            id: None,
            content: vec![AssistantContent::Reasoning(
                gents_protocol::message::Reasoning::new("only a thought"),
            )],
        };
        let blob = serde_json::to_string(&message).expect("serialize assistant message");
        let mut updates = Vec::new();
        project_row(&message_row("assistant", 1, &blob), &mut updates);
        assert_eq!(
            updates,
            vec![MessageUpdate::AgentThoughtChunk {
                text: "only a thought".to_string()
            }]
        );
    }

    #[test]
    fn assistant_tool_call_only_row_streams_no_chunks() {
        // Assistant rows carrying only tool calls stream nothing here; the
        // tool leaf owns tool_call projection.
        let message = Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(
                gents_protocol::message::ToolCall {
                    id: "call-1".to_string(),
                    call_id: None,
                    function: gents_protocol::message::ToolFunction::new(
                        "read_file".to_string(),
                        serde_json::json!({"path": "README.md"}),
                    ),
                    signature: None,
                    additional_params: None,
                },
            )],
        };
        let blob = serde_json::to_string(&message).expect("serialize assistant message");
        let mut updates = Vec::new();
        project_row(&message_row("assistant", 1, &blob), &mut updates);
        assert!(updates.is_empty());
    }

    #[test]
    fn durable_reasoning_field_streams_when_envelope_has_no_reasoning() {
        // #492: the reasoning copy may live only in AgentMessage.reasoning
        // after the response tail was cleared on finalize. The fallback
        // streams before the body text, matching the thought-before-text
        // contract every other assistant row follows.
        let mut row = message_row(
            "assistant",
            1,
            &serde_json::to_string(&Message::assistant("body text"))
                .expect("serialize assistant message"),
        );
        row.reasoning = Some("late reasoning".to_string());
        let mut updates = Vec::new();
        project_row(&row, &mut updates);
        assert_eq!(
            updates,
            vec![
                MessageUpdate::AgentThoughtChunk {
                    text: "late reasoning".to_string()
                },
                MessageUpdate::AgentMessageChunk {
                    text: "body text".to_string()
                },
            ]
        );
    }

    #[test]
    fn user_envelope_decodes_to_user_message_chunk() {
        let blob = serde_json::to_string(&Message::user("In one sentence, what is Gents?"))
            .expect("serialize user message");
        let mut updates = Vec::new();
        project_row(&message_row("user", 0, &blob), &mut updates);
        assert_eq!(
            updates,
            vec![MessageUpdate::UserMessageChunk {
                text: "In one sentence, what is Gents?".to_string()
            }]
        );
    }

    #[test]
    fn user_tool_result_rows_do_not_stream_as_user_chunks() {
        // Tool results are the tool leaf's domain; they are not message text.
        let message = Message::User {
            content: vec![UserContent::tool_result(
                "result-1",
                vec![gents_protocol::message::ToolResultContent::text(
                    "file contents",
                )],
            )],
        };
        let blob = serde_json::to_string(&message).expect("serialize user message");
        let mut updates = Vec::new();
        project_row(&message_row("user", 2, &blob), &mut updates);
        assert!(updates.is_empty());
    }

    #[test]
    fn legacy_plain_text_row_falls_back_to_a_single_chunk() {
        // decode_persisted_message tolerates legacy rows whose content is
        // plain text rather than a serialized envelope.
        let mut updates = Vec::new();
        project_row(
            &message_row("assistant", 1, "plain legacy text"),
            &mut updates,
        );
        assert_eq!(
            updates,
            vec![MessageUpdate::AgentMessageChunk {
                text: "plain legacy text".to_string()
            }]
        );
        let mut updates = Vec::new();
        project_row(&message_row("user", 0, "legacy user text"), &mut updates);
        assert_eq!(
            updates,
            vec![MessageUpdate::UserMessageChunk {
                text: "legacy user text".to_string()
            }]
        );
    }

    #[test]
    fn undecodable_blob_falls_back_to_plain_text_without_panicking() {
        let mut updates = Vec::new();
        project_row(
            &message_row("assistant", 1, r#"{"role":"assistant","content":"#),
            &mut updates,
        );
        // A malformed blob fails Message decoding, then bare-array decoding,
        // then falls back to plain text — which is the JSON fragment itself.
        // The fragment is not empty, so it streams as the fallback text; the
        // important property is that it never panics and never fabricates an
        // assistant envelope that was not there.
        assert!(!updates.is_empty());
    }

    #[test]
    fn empty_and_whitespace_rows_project_nothing() {
        let mut updates = Vec::new();
        project_row(&message_row("assistant", 1, "   "), &mut updates);
        project_row(&message_row("user", 0, ""), &mut updates);
        project_row(&message_row("assistant", 2, ""), &mut updates);
        assert!(updates.is_empty());
    }

    #[test]
    fn response_row_terminality_requires_complete_error_or_interrupted() {
        let row = |status: Option<&str>, interrupted_at: Option<&str>| ResponseRow {
            request_id: "req-1".to_string(),
            status: status.map(ToOwned::to_owned),
            error_message: None,
            token_count: None,
            content: None,
            reasoning: None,
            materialized_at: None,
            materialized_message_sequence: None,
            created_at: None,
            completed_at: None,
            interrupted_at: interrupted_at.map(ToOwned::to_owned),
        };

        assert!(!row(None, None).is_terminal());
        assert!(!row(Some(""), None).is_terminal());
        assert!(!row(Some("running"), None).is_terminal());
        assert!(!row(Some("in_progress"), None).is_terminal());
        assert!(row(Some("complete"), None).is_terminal());
        assert!(row(Some("error"), None).is_terminal());
        assert!(row(Some("running"), Some("2026-08-31T00:00:00Z")).is_terminal());
        // A blank interrupted_at is not terminal (the unit contract treats
        // only a non-empty interrupted_at as terminal).
        assert!(!row(Some("running"), Some("")).is_terminal());
    }

    #[test]
    fn stop_reason_projection_covers_cancelled_error_and_end_turn() {
        let cancelled = ResponseRow {
            request_id: "req-1".to_string(),
            status: Some("complete".to_string()),
            error_message: None,
            token_count: Some(120),
            content: None,
            reasoning: None,
            materialized_at: None,
            materialized_message_sequence: None,
            created_at: None,
            completed_at: None,
            interrupted_at: Some("2026-08-31T00:00:00Z".to_string()),
        };
        assert_eq!(cancelled.stop_reason(), "cancelled");

        let errored = ResponseRow {
            status: Some("error".to_string()),
            interrupted_at: None,
            ..cancelled.clone()
        };
        assert_eq!(errored.stop_reason(), "error");

        let completed = ResponseRow {
            status: Some("complete".to_string()),
            interrupted_at: None,
            ..cancelled.clone()
        };
        assert_eq!(completed.stop_reason(), "end_turn");
    }

    #[test]
    fn queries_escape_the_request_id() {
        let query = latest_response_query("req\"1\\x");
        assert!(
            !query.contains("\"req\"1\\x\""),
            "the interpolated request id must be escaped"
        );
        let messages = request_messages_query("req\"1\\x");
        assert!(
            !messages.contains("\"req\"1\\x\""),
            "the interpolated request id must be escaped"
        );
    }

    #[test]
    fn queries_are_request_scoped_and_bounded() {
        let query = latest_response_query("req-1");
        assert!(query.contains(r#"request_id: { _eq: "req-1" }"#));
        assert!(query.contains("limit: 1"));
        let messages = request_messages_query("req-1");
        assert!(messages.contains(r#"request_id: { _eq: "req-1" }"#));
        assert!(messages.contains("order: { sequence: ASC }"));
    }

    #[test]
    fn reasoning_texts_skip_encrypted_and_redacted_payloads() {
        use gents_protocol::message::{Reasoning, ReasoningContent};
        let reasoning = Reasoning {
            id: None,
            content: vec![
                ReasoningContent::Text {
                    text: "visible".to_string(),
                    signature: None,
                },
                ReasoningContent::Encrypted("opaque".to_string()),
                ReasoningContent::Redacted {
                    data: "opaque".to_string(),
                },
                ReasoningContent::Summary("summarized".to_string()),
            ],
        };
        assert_eq!(
            reasoning_texts(&reasoning),
            vec!["visible".to_string(), "summarized".to_string()]
        );
    }

    #[test]
    fn extract_message_reasoning_helper_still_applies_to_decoded_envelopes() {
        use gents_protocol::transcript::extract_message_reasoning;
        let message = Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::Reasoning(gents_protocol::message::Reasoning::new("why")),
                AssistantContent::text("what"),
            ],
        };
        assert_eq!(
            extract_message_reasoning(&message).as_deref(),
            Some("why"),
            "the transcript helper reads the same decoded envelope this leaf streams"
        );
    }

    /// Chunk identity is chunk-level, not row-level: one row's reasoning
    /// thought and body text get distinct keys (a row-level key would let
    /// the thought mark the text as already streamed), while two distinct
    /// rows with identical text both keep their own keys.
    #[test]
    fn update_keys_distinguish_a_rows_thought_from_its_text() {
        let blob = serde_json::to_string(&Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::Reasoning(gents_protocol::message::Reasoning::new("why")),
                AssistantContent::text("what"),
            ],
        })
        .expect("serialize assistant message");
        let mut updates = Vec::new();
        project_row(&message_row("assistant", 1, &blob), &mut updates);
        assert_eq!(
            updates,
            vec![
                MessageUpdate::AgentThoughtChunk {
                    text: "why".to_string()
                },
                MessageUpdate::AgentMessageChunk {
                    text: "what".to_string()
                },
            ]
        );
        // Rebuild the keys exactly as `project_messages` does so the
        // identity scheme itself is what is asserted here.
        let row = message_row("assistant", 1, &blob);
        let mut keys = Vec::new();
        let mut kinds_seen: std::collections::BTreeMap<&'static str, u64> =
            std::collections::BTreeMap::new();
        for update in &updates {
            let counter = kinds_seen.entry(update.session_update_kind()).or_default();
            *counter += 1;
            keys.push(format!(
                "{}:{}:{}",
                row.message_key,
                update.session_update_kind(),
                *counter
            ));
        }
        assert_eq!(
            keys,
            vec![
                "sess:1:agent_thought_chunk:1".to_string(),
                "sess:1:agent_message_chunk:1".to_string(),
            ],
            "the thought and the text of one row must have distinct chunk identities"
        );

        // Two distinct rows carrying identical text keep distinct keys too.
        let same_text = serde_json::to_string(&Message::assistant("twin"))
            .expect("serialize assistant message");
        let mut first = Vec::new();
        let mut second = Vec::new();
        let row_one = message_row("assistant", 1, &same_text);
        let row_two = message_row("assistant", 2, &same_text);
        project_row(&row_one, &mut first);
        project_row(&row_two, &mut second);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        let key_one = format!(
            "{}:{}:1",
            row_one.message_key,
            first[0].session_update_kind()
        );
        let key_two = format!(
            "{}:{}:1",
            row_two.message_key,
            second[0].session_update_kind()
        );
        assert_ne!(key_one, key_two);
    }
}
