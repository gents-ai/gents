use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde_json::{json, Map, Value};

pub(crate) mod caused_sessions;
mod child_output;
mod context;
pub(crate) mod messages;
pub(crate) mod tools;

pub(crate) const SESSION_UPDATE_METHOD: &str = "session/update";
pub(crate) const SUBAGENT_NOTIFICATION_METHOD: &str = "x.ai/session_notification";
pub(crate) const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = gents::DEFAULT_CONTEXT_WINDOW as u64;

pub(crate) fn effective_context_window_tokens(configured: u64) -> u64 {
    if configured == 0 {
        DEFAULT_CONTEXT_WINDOW_TOKENS
    } else {
        configured
    }
}

pub(super) fn nonempty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[derive(Debug, Clone)]
pub(crate) struct BoundModelContext {
    pub(crate) model_id: String,
    pub(crate) model_name: String,
    pub(crate) total_context_tokens: u64,
}

impl BoundModelContext {
    pub(crate) fn new(model_id: String, model_name: String, total_context_tokens: u64) -> Self {
        Self {
            model_id,
            model_name,
            total_context_tokens,
        }
    }

    pub(crate) fn effective_context_window(&self) -> u64 {
        effective_context_window_tokens(self.total_context_tokens)
    }
}

/// Only the most recent event id can roll back: downstream consumers treat
/// gaps as harmless and duplicates as not.
#[derive(Debug, Default)]
pub(crate) struct ProjectionSequencer {
    sessions: std::sync::Mutex<BTreeMap<String, SessionSequence>>,
}

#[derive(Debug, Default)]
struct SessionSequence {
    event_counter: u64,
    total_tokens: u64,
    context_order: Option<context::ContextOrder>,
}

pub(crate) struct EventIdReservation {
    sequencer: Arc<ProjectionSequencer>,
    session_id: String,
    value: u64,
    committed: bool,
}

impl EventIdReservation {
    pub(crate) fn event_id(&self) -> String {
        format!("{}-{}", self.session_id, self.value)
    }

    pub(crate) fn commit(mut self) {
        self.committed = true;
    }
}
impl Drop for EventIdReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.sequencer
                .rollback_event_id(&self.session_id, self.value);
        }
    }
}

impl ProjectionSequencer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn reserve_event_id(sequencer: &Arc<Self>, session_id: &str) -> EventIdReservation {
        let value = {
            let mut sessions = sequencer
                .sessions
                .lock()
                .expect("grok shim sequencer lock poisoned");
            let sequence = sessions.entry(session_id.to_string()).or_default();
            sequence.event_counter += 1;
            sequence.event_counter
        };
        EventIdReservation {
            sequencer: sequencer.clone(),
            session_id: session_id.to_string(),
            value,
            committed: false,
        }
    }

    fn rollback_event_id(&self, session_id: &str, value: u64) {
        let mut sessions = self
            .sessions
            .lock()
            .expect("grok shim sequencer lock poisoned");
        if let Some(sequence) = sessions.get_mut(session_id) {
            if sequence.event_counter == value {
                sequence.event_counter = sequence.event_counter.saturating_sub(1);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn event_counter(&self, session_id: &str) -> u64 {
        self.sessions
            .lock()
            .expect("grok shim sequencer lock poisoned")
            .get(session_id)
            .map(|sequence| sequence.event_counter)
            .unwrap_or(0)
    }

    pub(crate) fn session_total_tokens(&self, session_id: &str) -> u64 {
        self.sessions
            .lock()
            .expect("grok shim sequencer lock poisoned")
            .get(session_id)
            .map(|sequence| sequence.total_tokens)
            .unwrap_or(0)
    }

    fn observe_context(&self, session_id: &str, sample: context::ContextSample) {
        let mut sessions = self
            .sessions
            .lock()
            .expect("grok shim sequencer lock poisoned");
        let sequence = sessions.entry(session_id.to_owned()).or_default();
        match sequence
            .context_order
            .as_ref()
            .map(|order| sample.order.cmp(order))
        {
            Some(std::cmp::Ordering::Less) => return,
            Some(std::cmp::Ordering::Equal) => {
                sequence.total_tokens = sequence.total_tokens.max(sample.used)
            }
            _ => {
                sequence.total_tokens = sample.used;
                sequence.context_order = Some(sample.order);
            }
        }
    }
}

pub(crate) fn stamp_update_meta(
    event_id: &str,
    total_tokens: u64,
    prompt_id: Option<&str>,
    is_replay: Option<bool>,
    timestamps: UpdateTimestamps,
) -> Value {
    let mut meta = Map::new();
    meta.insert("eventId".to_string(), Value::String(event_id.to_string()));
    meta.insert("totalTokens".to_string(), Value::from(total_tokens));
    if let Some(prompt_id) = prompt_id {
        meta.insert("promptId".to_string(), Value::String(prompt_id.to_string()));
    }
    if let Some(is_replay) = is_replay {
        meta.insert("isReplay".to_string(), Value::Bool(is_replay));
    }
    if let Some(value) = timestamps.agent_timestamp_ms {
        meta.insert("agentTimestampMs".to_string(), Value::from(value));
    }
    if let Some(value) = timestamps.stream_start_ms {
        meta.insert("streamStartMs".to_string(), Value::from(value));
    }
    if let Some(value) = timestamps.turn_start_ms {
        meta.insert("turnStartMs".to_string(), Value::from(value));
    }
    Value::Object(meta)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UpdateTimestamps {
    pub(crate) agent_timestamp_ms: Option<i64>,
    pub(crate) stream_start_ms: Option<i64>,
    pub(crate) turn_start_ms: Option<i64>,
}
pub(crate) fn session_notification_for_method(
    method: &str,
    session_id: &str,
    update: Value,
    meta: Value,
) -> Value {
    let mut params = Map::new();
    params.insert(
        "sessionId".to_string(),
        Value::String(session_id.to_string()),
    );
    params.insert("update".to_string(), update);
    params.insert("_meta".to_string(), meta);
    json!({
        "jsonrpc": "2.0",
        "method": super::acp::wire_method(method),
        "params": Value::Object(params),
    })
}

/// The per-session send lock is held from before the event id is reserved
/// until after the notification was successfully enqueued and the
/// reservation committed, so allocation order equals enqueue order. The pager
/// deduplicates non-replay counters monotonically by `eventId`, so an
/// out-of-order delivery would silently drop the real update.
#[derive(Debug, Default)]
pub(crate) struct SessionUpdateChannel {
    sequencer: Arc<ProjectionSequencer>,
    send_locks: std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl SessionUpdateChannel {
    pub(crate) fn new(sequencer: Arc<ProjectionSequencer>) -> Self {
        Self {
            sequencer,
            send_locks: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    fn session_lock(&self, session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut send_locks = self
            .send_locks
            .lock()
            .expect("grok shim send-lock map poisoned");
        send_locks
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    async fn session_send_guard(&self, session_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        self.session_lock(session_id).lock_owned().await
    }

    pub(crate) async fn send(
        &self,
        session_id: &str,
        build_notification: impl FnOnce(&str, u64) -> Result<Value>,
        send_line: impl AsyncSendLine,
    ) -> Result<String> {
        self.send_with_commit(session_id, build_notification, send_line, NoCommit)
            .await
    }

    pub(crate) async fn send_with_commit(
        &self,
        session_id: &str,
        build_notification: impl FnOnce(&str, u64) -> Result<Value>,
        send_line: impl AsyncSendLine,
        commit: impl AsyncCommit,
    ) -> Result<String> {
        let _send_guard = self.session_send_guard(session_id).await;
        let reservation = ProjectionSequencer::reserve_event_id(&self.sequencer, session_id);
        let total_tokens = self.sequencer.session_total_tokens(session_id);
        let notification = build_notification(&reservation.event_id(), total_tokens)?;
        let line = serde_json::to_string(&notification)
            .context("serialize session/update notification")?;
        send_line.send_line(line.clone()).await?;
        reservation.commit();
        commit.commit().await;
        Ok(line)
    }
}

pub(crate) trait AsyncCommit: Send + Sync {
    async fn commit(&self);
}

impl<T: AsyncCommit + ?Sized> AsyncCommit for &T {
    async fn commit(&self) {
        (**self).commit().await
    }
}

struct NoCommit;

impl AsyncCommit for NoCommit {
    async fn commit(&self) {}
}

pub(crate) trait AsyncSendLine: Send + Sync {
    async fn send_line(&self, line: String) -> Result<()>;
}

impl<T: AsyncSendLine + ?Sized> AsyncSendLine for &T {
    async fn send_line(&self, line: String) -> Result<()> {
        (**self).send_line(line).await
    }
}

impl<T: AsyncSendLine + ?Sized> AsyncSendLine for Arc<T> {
    async fn send_line(&self, line: String) -> Result<()> {
        (**self).send_line(line).await
    }
}

pub(crate) struct ProjectionEngine {
    pub(crate) background_executions: gents::hook::BackgroundExecutionRegistry,
    node: Arc<EmbeddedNode>,
    bound: BoundModelContext,
    sequencer: Arc<ProjectionSequencer>,
    channel: SessionUpdateChannel,
    /// Tool-call ids of the job runs this connection holds a live frame for,
    /// by observed session.
    delivered_job_runs:
        std::sync::Mutex<std::collections::BTreeMap<String, std::collections::BTreeSet<String>>>,
}

impl ProjectionEngine {
    pub(crate) fn new(node: Arc<EmbeddedNode>, bound: BoundModelContext) -> Self {
        let sequencer = Arc::new(ProjectionSequencer::new());
        Self {
            node,
            bound,
            background_executions: Default::default(),
            channel: SessionUpdateChannel::new(sequencer.clone()),
            sequencer,
            delivered_job_runs: Default::default(),
        }
    }

    pub(super) fn record_job_runs(&self, session: &str, runs: &std::collections::BTreeSet<String>) {
        self.delivered_job_runs
            .lock()
            .unwrap()
            .insert(session.to_owned(), runs.clone());
    }

    pub(super) fn delivered_job_runs(&self) -> std::collections::BTreeSet<String> {
        self.delivered_job_runs
            .lock()
            .unwrap()
            .values()
            .flatten()
            .cloned()
            .collect()
    }

    pub(crate) fn session_updates(&self) -> &SessionUpdateChannel {
        &self.channel
    }

    pub(crate) fn with_background_executions(
        mut self,
        executions: gents::hook::BackgroundExecutionRegistry,
    ) -> Self {
        self.background_executions = executions;
        self
    }

    #[cfg(test)]
    pub(crate) fn sequencer_arc(&self) -> Arc<ProjectionSequencer> {
        self.sequencer.clone()
    }

    pub(crate) async fn project_request_updates(
        &self,
        request: &gents_protocol::row::AgentRequestRow,
        cursor: &mut RequestCursor,
        parent_prompt_id: Option<&str>,
    ) -> Result<ProjectionBatch> {
        let session_id = request
            .session_id
            .as_deref()
            .context("projection request session missing")?;
        anyhow::ensure!(
            request.doc_id.as_deref().is_some_and(|id| !id.is_empty())
                && request.node_did.as_deref().is_some_and(|id| !id.is_empty()),
            "projection requires actual scoped physical request"
        );
        let mut merged: Vec<MergedEvent> = Vec::new();

        let message_sequence_high_water = cursor.message_sequence_high_water;
        let messages = messages::project_messages(
            &self.node,
            message_sequence_high_water,
            request,
            self.bound.effective_context_window(),
        )
        .await?;
        cursor.observe_timestamps(&messages);
        if let Some(sample) = context::load(&self.node, request).await? {
            self.sequencer.observe_context(session_id, sample);
        }
        let durable_rows = durable_row_views(&messages);
        let mut planned_live = cursor.live_cursors.clone();
        {
            for (kind, observed, is_reasoning) in [
                (
                    messages::AGENT_THOUGHT_CHUNK,
                    messages.live_tail.reasoning.as_deref().unwrap_or_default(),
                    true,
                ),
                (
                    messages::AGENT_MESSAGE_CHUNK,
                    messages.live_tail.content.as_deref().unwrap_or_default(),
                    false,
                ),
            ] {
                let live = if is_reasoning {
                    &mut planned_live.reasoning
                } else {
                    &mut planned_live.content
                };
                if let Some(source_key) = messages.live_tail.source_key.as_deref() {
                    live.begin_source(source_key);
                }
                let Some((delta, mut plan)) = live.plan(observed) else {
                    continue;
                };
                plan.segment_key.get_or_insert_with(|| {
                    messages.live_tail.source_key.clone().unwrap_or_else(|| {
                        format!(
                            "canonical:{}:{kind}",
                            request.doc_id.as_deref().unwrap_or_default()
                        )
                    })
                });
                live.commit(plan.clone());
                if delta.is_empty() {
                    continue;
                }
                let advance = if is_reasoning {
                    CursorAdvance::LiveReasoning { plan }
                } else {
                    CursorAdvance::LiveContent { plan }
                };
                merged.push(MergedEvent {
                    event: NovelProjectionEvent {
                        method: SESSION_UPDATE_METHOD,
                        payload: messages::MessageUpdate::chunk_payload(kind, delta),
                        timing: None,
                        advance,
                    },
                    chronology: messages.live_tail.assistant_sequence,
                    family_rank: FAMILY_RANK_MESSAGE,
                    family_ordinal: merged
                        .iter()
                        .filter(|event| event.family_rank == FAMILY_RANK_MESSAGE)
                        .count(),
                });
            }
            for binding in &messages.canonical_bindings {
                let identity = DurableRowIdentity {
                    sequence: binding.sequence,
                    message_key: binding.message_key.clone(),
                };
                for (rail, live) in [
                    (EvidenceRail::Content, &mut planned_live.content),
                    (EvidenceRail::Reasoning, &mut planned_live.reasoning),
                ] {
                    bind_canonical_source_evidence(live, binding, &identity, rail, &durable_rows);
                }
            }
        }
        let tools = tools::project_tools(&self.node, request).await?;
        for (index, update) in tools.updates.iter().enumerate() {
            let chronology = tools.chronology.get(index).copied().flatten();
            match update {
                tools::ToolUpdate::ToolCall(base) => {
                    let payload = base.to_payload();
                    let Some((emitted, advance)) =
                        cursor.tool_base_novel(&base.tool_call_id, &payload)
                    else {
                        continue;
                    };
                    merged.push(MergedEvent {
                        event: NovelProjectionEvent {
                            method: SESSION_UPDATE_METHOD,
                            payload: emitted,
                            timing: None,
                            advance,
                        },
                        chronology,
                        family_rank: FAMILY_RANK_TOOL,
                        family_ordinal: merged
                            .iter()
                            .filter(|item| item.family_rank == FAMILY_RANK_TOOL)
                            .count(),
                    });
                }
                tools::ToolUpdate::ToolCallUpdate(update) => {
                    let status = update
                        .fields
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let Some(advance) = cursor.tool_terminal_novel(&update.tool_call_id, status)
                    else {
                        continue;
                    };
                    merged.push(MergedEvent {
                        event: NovelProjectionEvent {
                            method: SESSION_UPDATE_METHOD,
                            payload: tools::tool_call_update_payload(
                                &update.tool_call_id,
                                &update.fields,
                            ),
                            timing: None,
                            advance,
                        },
                        chronology,
                        family_rank: FAMILY_RANK_TOOL,
                        family_ordinal: merged
                            .iter()
                            .filter(|item| item.family_rank == FAMILY_RANK_TOOL)
                            .count(),
                    });
                }
                tools::ToolUpdate::BackgroundTask(update) => {
                    let advance = if update.kind == "tool_call_update" {
                        let fingerprint =
                            payload_fingerprint(&json!([update.payload, update.output_start]));
                        (cursor.background_outputs.get(&update.key) != Some(&fingerprint)).then(
                            || CursorAdvance::BackgroundOutput {
                                key: update.key.clone(),
                                fingerprint,
                                output_start: update.output_start,
                            },
                        )
                    } else {
                        cursor.background_task_novel(&update.key)
                    };
                    let Some(advance) = advance else {
                        continue;
                    };
                    merged.push(MergedEvent {
                        event: NovelProjectionEvent {
                            method: update.method,
                            payload: update.payload.clone(),
                            timing: None,
                            advance,
                        },
                        chronology,
                        family_rank: FAMILY_RANK_TOOL,
                        family_ordinal: merged
                            .iter()
                            .filter(|item| item.family_rank == FAMILY_RANK_TOOL)
                            .count(),
                    });
                }
                tools::ToolUpdate::AvailableCommands(update) => {
                    let payload = update.to_payload();
                    let fingerprint = payload_fingerprint(&payload);
                    let Some(advance) = cursor.commands_changed(fingerprint) else {
                        continue;
                    };
                    merged.push(MergedEvent {
                        event: NovelProjectionEvent {
                            method: SESSION_UPDATE_METHOD,
                            payload,
                            timing: None,
                            advance,
                        },
                        chronology,
                        family_rank: FAMILY_RANK_TOOL,
                        family_ordinal: merged
                            .iter()
                            .filter(|item| item.family_rank == FAMILY_RANK_TOOL)
                            .count(),
                    });
                }
            }
        }

        let subagents = caused_sessions::project_caused_sessions(
            &self.node,
            request,
            parent_prompt_id,
            self.bound.effective_context_window(),
            &self.delivered_job_runs(),
        )
        .await?;
        for (index, update) in subagents.updates.iter().enumerate() {
            let chronology = subagents.chronology.get(index).copied().flatten();
            let payload = update.to_payload();
            let key = format!("{}:{}", update.session_update_kind(), update.subagent_id());
            let fingerprint = payload_fingerprint(&payload);
            let Some(advance) = cursor.subagent_changed(&key, fingerprint) else {
                continue;
            };
            merged.push(MergedEvent {
                event: NovelProjectionEvent {
                    method: SUBAGENT_NOTIFICATION_METHOD,
                    payload,
                    timing: None,
                    advance,
                },
                chronology,
                family_rank: FAMILY_RANK_CAUSED_SESSION,
                family_ordinal: merged
                    .iter()
                    .filter(|item| item.family_rank == FAMILY_RANK_CAUSED_SESSION)
                    .count(),
            });
        }

        let mut durable_trailing = Vec::new();
        {
            let mut planned_durable = cursor.durable_chunks.clone();
            let mut row_texts: BTreeMap<(DurableRowIdentity, EvidenceRail), String> =
                BTreeMap::new();
            for (index, update) in messages.updates.iter().enumerate() {
                let Some(sequence) = messages.chronology.get(index).copied().flatten() else {
                    continue;
                };
                let (rail, text) = match update {
                    messages::MessageUpdate::AgentMessageChunk { text } => {
                        (EvidenceRail::Content, text)
                    }
                    messages::MessageUpdate::AgentThoughtChunk { text } => {
                        (EvidenceRail::Reasoning, text)
                    }
                    messages::MessageUpdate::UserMessageChunk { .. } => continue,
                };
                let Some(key) = messages.update_keys.get(index) else {
                    continue;
                };
                let Some(message_key) = durable_message_key(key, update.session_update_kind())
                else {
                    continue;
                };
                row_texts
                    .entry((
                        DurableRowIdentity {
                            sequence,
                            message_key: message_key.to_string(),
                        },
                        rail,
                    ))
                    .or_default()
                    .push_str(text);
            }

            let mut row_offsets: BTreeMap<(DurableRowIdentity, EvidenceRail), usize> =
                BTreeMap::new();
            for (index, update) in messages.updates.iter().enumerate() {
                let Some(key) = messages.update_keys.get(index) else {
                    continue;
                };
                if key.trim().is_empty() {
                    continue;
                }
                let chronology = messages.chronology.get(index).copied().flatten();
                if let messages::MessageUpdate::UserMessageChunk { text } = update {
                    let is_notification = durable_message_key(key, update.session_update_kind())
                        .is_some_and(gents::background_completion::is_background_completion_notification_message_key);
                    if is_notification {
                        let planned = planned_durable.entry(key.clone()).or_default();
                        let sent_len = text
                            .strip_prefix(&planned.sent_text)
                            .map(|suffix| text.len() - suffix.len())
                            .unwrap_or(0);
                        if sent_len < text.len() {
                            merged.push(MergedEvent {
                                event: NovelProjectionEvent {
                                    method: SESSION_UPDATE_METHOD,
                                    payload: messages::MessageUpdate::background_completion_payload(
                                        &text[sent_len..],
                                    ),
                                    timing: None,
                                    advance: CursorAdvance::DurableChunk {
                                        message_key: key.clone(),
                                        sent_text: text.clone(),
                                    },
                                },
                                chronology,
                                family_rank: FAMILY_RANK_MESSAGE,
                                family_ordinal: index,
                            });
                            planned.sent_text = text.clone();
                        }
                    }
                    continue;
                }
                let (rail, text) = match update {
                    messages::MessageUpdate::AgentMessageChunk { text } => {
                        (EvidenceRail::Content, text)
                    }
                    messages::MessageUpdate::AgentThoughtChunk { text } => {
                        (EvidenceRail::Reasoning, text)
                    }
                    messages::MessageUpdate::UserMessageChunk { .. } => continue,
                };
                let live = match rail {
                    EvidenceRail::Reasoning => &planned_live.reasoning,
                    EvidenceRail::Content => &planned_live.content,
                };
                let row_identity = chronology.and_then(|sequence| {
                    durable_message_key(key, update.session_update_kind()).map(|message_key| {
                        DurableRowIdentity {
                            sequence,
                            message_key: message_key.to_string(),
                        }
                    })
                });
                let bound_evidence = chronology.and_then(|_| {
                    live.closed_evidence
                        .iter()
                        .find(|evidence| evidence.bound_row.as_ref() == row_identity.as_ref())
                });
                let evidence = bound_evidence
                    .map(|evidence| evidence.sent_bytes.as_str())
                    .unwrap_or_default();
                let row_offset = row_identity
                    .clone()
                    .map(|identity| {
                        let offset = row_offsets.entry((identity, rail)).or_default();
                        let current = *offset;
                        *offset = offset.saturating_add(text.len());
                        current
                    })
                    .unwrap_or_default();
                let evidence_covered = evidence.len().saturating_sub(row_offset).min(text.len());
                let planned = planned_durable.entry(key.clone()).or_default();
                let mut sent_len = if text.starts_with(&planned.sent_text) {
                    planned.sent_text.len()
                } else {
                    0
                };
                if evidence_covered > sent_len
                    && text.is_char_boundary(evidence_covered)
                    && row_identity
                        .as_ref()
                        .and_then(|identity| row_texts.get(&(identity.clone(), rail)))
                        .is_some_and(|row_text| row_text.starts_with(evidence))
                {
                    sent_len = evidence_covered;
                }
                if sent_len >= text.len() {
                    planned.sent_text = text.clone();
                    durable_trailing.push(CursorAdvance::DurableChunk {
                        message_key: key.clone(),
                        sent_text: text.clone(),
                    });
                    continue;
                }
                let suffix = text[sent_len..].to_string();
                let payload_kind = update.session_update_kind();
                let segment_key = bound_evidence
                    .and_then(|evidence| evidence.segment_key.clone())
                    .or_else(|| {
                        row_identity.as_ref().map(|identity| {
                            format!("message:{}:{}", identity.sequence, identity.message_key)
                        })
                    });
                let timing = segment_key
                    .map(|segment_key| cursor.timing_for_segment(segment_key, chronology));
                merged.push(MergedEvent {
                    event: NovelProjectionEvent {
                        method: SESSION_UPDATE_METHOD,
                        payload: messages::MessageUpdate::chunk_payload(payload_kind, suffix),
                        timing,
                        advance: CursorAdvance::DurableChunk {
                            message_key: key.clone(),
                            sent_text: text.clone(),
                        },
                    },
                    chronology,
                    family_rank: FAMILY_RANK_MESSAGE,
                    family_ordinal: merged
                        .iter()
                        .filter(|item| item.family_rank == FAMILY_RANK_MESSAGE)
                        .count(),
                });
                planned.sent_text = text.clone();
            }
        }

        merged.sort_by(|a, b| family_sort_key(a).cmp(&family_sort_key(b)));
        let events: Vec<NovelProjectionEvent> = merged.into_iter().map(|item| item.event).collect();
        let mut trailing_advances = durable_trailing;
        if let Some(sequence) = messages.message_sequence_high_water {
            trailing_advances.push(CursorAdvance::MessageHighWater { sequence });
        }
        Ok(ProjectionBatch {
            events,
            trailing_advances,
        })
    }
}

const FAMILY_RANK_MESSAGE: u8 = 0;
const FAMILY_RANK_TOOL: u8 = 1;
const FAMILY_RANK_CAUSED_SESSION: u8 = 2;

struct MergedEvent {
    event: NovelProjectionEvent,
    chronology: Option<i64>,
    family_rank: u8,
    family_ordinal: usize,
}

fn family_sort_key(event: &MergedEvent) -> (i64, u8, usize) {
    (
        event.chronology.unwrap_or(i64::MAX),
        event.family_rank,
        event.family_ordinal,
    )
}

#[derive(Debug, Clone)]
pub(crate) struct NovelProjectionEvent {
    pub(crate) method: &'static str,
    pub(crate) payload: Value,
    pub(crate) timing: Option<ProjectionEventTiming>,
    pub(crate) advance: CursorAdvance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionEventTiming {
    pub(crate) segment_key: String,
    pub(crate) stream_start_candidate_ms: Option<i64>,
    pub(crate) agent_timestamp_candidate_ms: Option<i64>,
}

#[derive(Debug, Default)]
pub(crate) struct ProjectionBatch {
    pub(crate) events: Vec<NovelProjectionEvent>,
    pub(crate) trailing_advances: Vec<CursorAdvance>,
}

impl std::ops::Deref for ProjectionBatch {
    type Target = [NovelProjectionEvent];

    fn deref(&self) -> &Self::Target {
        &self.events
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CursorAdvance {
    Many(Vec<CursorAdvance>),
    ToolBase {
        tool_call_id: String,
        payload: Value,
    },
    ToolTerminal {
        tool_call_id: String,
        status: String,
    },
    Commands {
        fingerprint: u64,
    },
    CausedSession {
        key: String,
        fingerprint: u64,
    },
    BackgroundTask {
        key: String,
    },
    BackgroundOutput {
        key: String,
        fingerprint: u64,
        output_start: Option<u64>,
    },
    ChildOutput {
        key: String,
        receipt: child_output::OutputReceipt,
    },
    LiveContent {
        plan: LiveSegmentPlan,
    },
    LiveReasoning {
        plan: LiveSegmentPlan,
    },
    DurableChunk {
        message_key: String,
        sent_text: String,
    },
    MessageHighWater {
        sequence: i64,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LiveSegmentPlan {
    segment_key: Option<String>,
    observed: String,
    sent_len: usize,
    sent_bytes: String,
    closed_evidence: Vec<ClosedEvidence>,
}

/// A divergence (the freshly observed tail no longer starts with the
/// previously observed snapshot) never slices into the sent prefix; the whole
/// freshly observed snapshot opens a new segment, dropping the un-sent
/// remainder the runtime retracted.
#[derive(Clone, Debug, Default)]
pub(crate) struct LiveSegmentCursor {
    segment_key: Option<String>,
    observed: String,
    sent_len: usize,
    sent_bytes: String,
    closed_evidence: Vec<ClosedEvidence>,
}
#[derive(Clone, Debug, Default)]
struct LiveCursorPair {
    reasoning: LiveSegmentCursor,
    content: LiveSegmentCursor,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct DurableRowIdentity {
    sequence: i64,
    message_key: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ClosedEvidence {
    sent_bytes: String,
    bound_row: Option<DurableRowIdentity>,
    segment_key: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct DurableRowView {
    identity: DurableRowIdentity,
    content: String,
    reasoning: String,
}

fn durable_message_key<'a>(update_key: &'a str, kind: &str) -> Option<&'a str> {
    let (prefix, ordinal) = update_key.rsplit_once(':')?;
    ordinal.parse::<u64>().ok()?;
    prefix.strip_suffix(&format!(":{kind}"))
}

fn durable_row_views(messages: &messages::MessageProjection) -> Vec<DurableRowView> {
    let mut rows: BTreeMap<DurableRowIdentity, DurableRowView> = BTreeMap::new();
    for (index, update) in messages.updates.iter().enumerate() {
        let Some(sequence) = messages.chronology.get(index).copied().flatten() else {
            continue;
        };
        let Some(update_key) = messages.update_keys.get(index) else {
            continue;
        };
        let kind = update.session_update_kind();
        let Some(message_key) = durable_message_key(update_key, kind) else {
            continue;
        };
        let identity = DurableRowIdentity {
            sequence,
            message_key: message_key.to_string(),
        };
        let row = rows
            .entry(identity.clone())
            .or_insert_with(|| DurableRowView {
                identity,
                ..DurableRowView::default()
            });
        match update {
            messages::MessageUpdate::AgentMessageChunk { text } => row.content.push_str(text),
            messages::MessageUpdate::AgentThoughtChunk { text } => row.reasoning.push_str(text),
            messages::MessageUpdate::UserMessageChunk { .. } => {}
        }
    }
    rows.into_values().collect()
}

fn upsert_bound_evidence(
    evidence: &mut Vec<ClosedEvidence>,
    sent_bytes: String,
    row: DurableRowIdentity,
    segment_key: Option<String>,
) {
    if let Some(existing) = evidence
        .iter_mut()
        .find(|item| item.bound_row.as_ref() == Some(&row))
    {
        if sent_bytes.starts_with(&existing.sent_bytes) {
            existing.sent_bytes = sent_bytes;
        }
        existing.segment_key = existing.segment_key.clone().or(segment_key);
        return;
    }
    evidence.push(ClosedEvidence {
        sent_bytes,
        bound_row: Some(row),
        segment_key,
    });
}

fn bind_open_tip_evidence(
    live: &mut LiveSegmentCursor,
    row: &DurableRowIdentity,
    rail: EvidenceRail,
    rows: &[DurableRowView],
) {
    if live.sent_bytes.is_empty() {
        return;
    }
    let Some(view) = rows.iter().find(|view| &view.identity == row) else {
        return;
    };
    let durable = match rail {
        EvidenceRail::Content => &view.content,
        EvidenceRail::Reasoning => &view.reasoning,
    };
    if !durable.starts_with(&live.sent_bytes) {
        return;
    }
    let sent_bytes = live.sent_bytes.clone();
    let segment_key = live.segment_key.clone();
    upsert_bound_evidence(
        &mut live.closed_evidence,
        sent_bytes,
        row.clone(),
        segment_key,
    );
}

fn bind_canonical_source_evidence(
    live: &mut LiveSegmentCursor,
    binding: &messages::CanonicalSourceBinding,
    row: &DurableRowIdentity,
    rail: EvidenceRail,
    rows: &[DurableRowView],
) {
    if live.segment_key.as_deref() != Some(binding.source_key.as_str())
        || binding.sequence != row.sequence
        || binding.message_key != row.message_key
    {
        return;
    }
    bind_open_tip_evidence(live, row, rail, rows);
}

impl LiveSegmentCursor {
    fn begin_source(&mut self, source_key: &str) {
        if self.segment_key.as_deref() == Some(source_key) {
            return;
        }
        if !self.sent_bytes.is_empty() {
            self.closed_evidence.push(ClosedEvidence {
                sent_bytes: self.sent_bytes.clone(),
                bound_row: None,
                segment_key: self.segment_key.clone(),
            });
        }
        self.segment_key = Some(source_key.to_string());
        self.observed.clear();
        self.sent_len = 0;
        self.sent_bytes.clear();
    }

    fn plan(&self, observed: &str) -> Option<(String, LiveSegmentPlan)> {
        if observed == self.observed {
            return None;
        }
        if let Some(suffix) = observed.strip_prefix(&self.observed) {
            let mut sent_bytes = self.sent_bytes.clone();
            sent_bytes.push_str(suffix);
            return Some((
                suffix.to_string(),
                LiveSegmentPlan {
                    segment_key: self.segment_key.clone(),
                    observed: observed.to_string(),
                    sent_len: self.sent_len + suffix.len(),
                    sent_bytes,
                    closed_evidence: self.closed_evidence.clone(),
                },
            ));
        }
        let mut closed_evidence = self.closed_evidence.clone();
        if !self.sent_bytes.is_empty() {
            closed_evidence.push(ClosedEvidence {
                sent_bytes: self.sent_bytes.clone(),
                bound_row: None,
                segment_key: self.segment_key.clone(),
            });
        }
        Some((
            observed.to_string(),
            LiveSegmentPlan {
                segment_key: self.segment_key.clone(),
                observed: observed.to_string(),
                sent_len: observed.len(),
                sent_bytes: observed.to_string(),
                closed_evidence,
            },
        ))
    }

    fn commit(&mut self, plan: LiveSegmentPlan) {
        self.segment_key = plan.segment_key;
        self.observed = plan.observed;
        self.sent_len = plan.sent_len;
        self.sent_bytes = plan.sent_bytes;
        self.closed_evidence = plan.closed_evidence;
    }
}

#[derive(Debug, Default)]
pub(crate) struct RequestCursor {
    pub(crate) request: Option<gents_protocol::row::AgentRequestRow>,
    tool_bases: BTreeMap<String, Value>,
    tool_terminal_states: BTreeMap<String, String>,
    commands_state: Option<u64>,
    subagent_states: BTreeMap<String, u64>,
    background_task_events: std::collections::BTreeSet<String>,
    background_outputs: BTreeMap<String, u64>,
    child_outputs: BTreeMap<String, child_output::OutputReceipt>,
    live_cursors: LiveCursorPair,
    durable_chunks: BTreeMap<String, DurableChunkState>,
    message_sequence_high_water: Option<i64>,
    response_started_at_ms: Option<i64>,
    response_ended_at_ms: Option<i64>,
    message_timestamps: BTreeMap<i64, (String, Option<i64>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum EvidenceRail {
    Content,
    Reasoning,
}

impl RequestCursor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn observe_timestamps(&mut self, messages: &messages::MessageProjection) {
        self.response_ended_at_ms = messages.response_ended_at_ms;
        if self.response_started_at_ms.is_none() {
            self.response_started_at_ms = messages.response_started_at_ms;
        }
        for row in &messages.timeline {
            let entry = self
                .message_timestamps
                .entry(row.sequence)
                .or_insert_with(|| (row.message_key.clone(), row.timestamp_ms));
            if entry.0 == row.message_key {
                entry.1 = match (entry.1, row.timestamp_ms) {
                    (Some(old), Some(new)) => Some(old.min(new)),
                    (old, new) => old.or(new),
                };
            }
        }
    }

    fn timing_for_segment(
        &self,
        segment_key: String,
        chronology: Option<i64>,
    ) -> ProjectionEventTiming {
        let stream_start_candidate_ms = chronology
            .and_then(|sequence| {
                self.message_timestamps
                    .range(..sequence)
                    .rev()
                    .find_map(|(_, (_, timestamp))| *timestamp)
            })
            .or_else(|| {
                chronology
                    .is_none()
                    .then(|| {
                        self.message_timestamps
                            .iter()
                            .rev()
                            .find_map(|(_, (_, timestamp))| *timestamp)
                    })
                    .flatten()
            })
            .or(self.response_started_at_ms);
        let agent_timestamp_candidate_ms = chronology
            .and_then(|sequence| {
                self.message_timestamps
                    .get(&sequence)
                    .and_then(|(_, timestamp)| *timestamp)
            })
            .or(self.response_ended_at_ms);
        ProjectionEventTiming {
            segment_key,
            stream_start_candidate_ms,
            agent_timestamp_candidate_ms,
        }
    }

    fn tool_base_novel(
        &mut self,
        tool_call_id: &str,
        payload: &Value,
    ) -> Option<(Value, CursorAdvance)> {
        let advance = CursorAdvance::ToolBase {
            tool_call_id: tool_call_id.to_string(),
            payload: payload.clone(),
        };
        match self.tool_bases.get(tool_call_id) {
            None => Some((payload.clone(), advance)),
            Some(last_sent) => {
                let mut fields = changed_tool_fields(last_sent, payload)?;
                if matches!(
                    payload.get("status").and_then(Value::as_str),
                    Some("completed" | "failed")
                ) {
                    fields.as_object_mut()?.remove("status");
                    if fields.as_object()?.is_empty() {
                        return None;
                    }
                }
                Some((
                    tools::tool_call_update_payload(tool_call_id, &fields),
                    advance,
                ))
            }
        }
    }

    fn commands_changed(&mut self, fingerprint: u64) -> Option<CursorAdvance> {
        if self.commands_state == Some(fingerprint) {
            return None;
        }
        Some(CursorAdvance::Commands { fingerprint })
    }

    fn tool_terminal_novel(&self, tool_call_id: &str, status: &str) -> Option<CursorAdvance> {
        if status.is_empty()
            || self
                .tool_terminal_states
                .get(tool_call_id)
                .map(String::as_str)
                == Some(status)
        {
            return None;
        }
        Some(CursorAdvance::ToolTerminal {
            tool_call_id: tool_call_id.to_string(),
            status: status.to_string(),
        })
    }

    pub(super) fn subagent_spawn_was_delivered(&self, child_session_id: &str) -> bool {
        self.subagent_states
            .contains_key(&format!("subagent_spawned:{child_session_id}"))
    }

    fn subagent_changed(&mut self, key: &str, fingerprint: u64) -> Option<CursorAdvance> {
        if self.subagent_states.get(key) == Some(&fingerprint) {
            return None;
        }
        Some(CursorAdvance::CausedSession {
            key: key.to_string(),
            fingerprint,
        })
    }

    fn background_task_novel(&self, key: &str) -> Option<CursorAdvance> {
        (!self.background_task_events.contains(key)).then(|| CursorAdvance::BackgroundTask {
            key: key.to_string(),
        })
    }

    pub(crate) fn record(&mut self, advance: CursorAdvance) {
        match advance {
            CursorAdvance::Many(advances) => {
                for advance in advances {
                    self.record(advance);
                }
            }
            CursorAdvance::ToolBase {
                tool_call_id,
                payload,
            } => {
                self.tool_bases.insert(tool_call_id, payload);
            }
            CursorAdvance::ToolTerminal {
                tool_call_id,
                status,
            } => {
                self.tool_terminal_states.insert(tool_call_id, status);
            }
            CursorAdvance::Commands { fingerprint } => {
                self.commands_state = Some(fingerprint);
            }
            CursorAdvance::CausedSession { key, fingerprint } => {
                self.subagent_states.insert(key, fingerprint);
            }
            CursorAdvance::BackgroundTask { key } => {
                self.background_task_events.insert(key);
            }
            CursorAdvance::BackgroundOutput {
                key, fingerprint, ..
            } => {
                self.background_outputs.insert(key, fingerprint);
            }
            CursorAdvance::ChildOutput { key, receipt } => {
                self.child_outputs.insert(key, receipt);
            }
            CursorAdvance::LiveContent { plan } => {
                self.live_cursors.content.commit(plan);
            }
            CursorAdvance::LiveReasoning { plan } => {
                self.live_cursors.reasoning.commit(plan);
            }
            CursorAdvance::DurableChunk {
                message_key,
                sent_text,
            } => {
                self.durable_chunks
                    .entry(message_key)
                    .or_default()
                    .sent_text = sent_text;
            }
            CursorAdvance::MessageHighWater { sequence } => {
                self.message_sequence_high_water = Some(
                    self.message_sequence_high_water
                        .map_or(sequence, |high| high.max(sequence)),
                );
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
struct DurableChunkState {
    sent_text: String,
}

const TRACKED_TOOL_FIELDS: [&str; 7] = [
    "title",
    "kind",
    "status",
    "content",
    "rawInput",
    "rawOutput",
    "_meta",
];

fn changed_tool_fields(last_sent: &Value, observed: &Value) -> Option<Value> {
    let last = last_sent.as_object()?;
    let fresh = observed.as_object()?;
    let mut fields = Map::new();
    for key in TRACKED_TOOL_FIELDS {
        let fresh_value = fresh.get(key);
        if fresh_value != last.get(key) {
            match fresh_value {
                Some(value) => {
                    fields.insert(key.to_string(), value.clone());
                }
                None => {
                    fields.insert(key.to_string(), Value::Null);
                }
            }
        }
    }
    if fields.is_empty() {
        None
    } else {
        Some(Value::Object(fields))
    }
}

fn payload_fingerprint(payload: &Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_value(&mut hasher, payload);
    hasher.finish()
}

fn hash_value<H: Hasher>(hasher: &mut H, value: &Value) {
    match value {
        Value::Null => 0u8.hash(hasher),
        Value::Bool(value) => {
            1u8.hash(hasher);
            value.hash(hasher);
        }
        Value::Number(value) => {
            2u8.hash(hasher);
            value.to_string().hash(hasher);
        }
        Value::String(value) => {
            3u8.hash(hasher);
            value.hash(hasher);
        }
        Value::Array(values) => {
            4u8.hash(hasher);
            for value in values {
                hash_value(hasher, value);
            }
        }
        Value::Object(fields) => {
            5u8.hash(hasher);
            for (key, value) in fields.iter().collect::<BTreeMap<_, _>>() {
                key.hash(hasher);
                hash_value(hasher, value);
            }
        }
    }
}

pub(crate) async fn resolve_bound_model_context(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
) -> Result<BoundModelContext> {
    use crate::commands::inference_binding::{load_bound_context_window, load_bound_profile};
    let profile = load_bound_profile(node, node_did, agent_id).await?;
    let context_window = load_bound_context_window(node, node_did, agent_id).await?;
    Ok(BoundModelContext::new(
        profile.model_name.clone(),
        profile.model_name,
        u64::try_from(context_window).context("invalid bound context window")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::grok_shim::test_fixtures::seed_canonical_assistant_message;
    use gents::graphql::ensure_no_errors;

    #[tokio::test]
    async fn chunk_level_identity_recovers_a_partial_row_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .data_path(dir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("embedded node"),
        );
        gents::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .expect("runtime schemas");

        let request_id = "req-chunk-retry";
        let request = seed_projection_request(&node, "s-chunk", request_id).await;
        let message_key = gents::session::sequence_message_key(
            request.node_did.as_deref().unwrap(),
            request.session_id.as_deref().unwrap(),
            request.requester_did.as_deref(),
            1,
        );
        seed_canonical_assistant_message(&node, &request, &message_key, 1, "thinking", "answer")
            .await;

        let engine = ProjectionEngine::new(
            node,
            BoundModelContext::new(
                "GLM-5.3-NVFP4".to_string(),
                "GLM-5.3-NVFP4".to_string(),
                262_144,
            ),
        );
        let mut cursor = RequestCursor::new();

        let first = engine
            .project_request_updates(&request, &mut cursor, None)
            .await
            .expect("first poll");
        assert_eq!(first.len(), 2, "thought plus text both stream");
        let kinds: Vec<&str> = first
            .iter()
            .map(|event| {
                event.payload["sessionUpdate"]
                    .as_str()
                    .expect("sessionUpdate kind")
            })
            .collect();
        assert_eq!(kinds, vec!["agent_thought_chunk", "agent_message_chunk"]);

        cursor.record(first[0].advance.clone());
        let second = engine
            .project_request_updates(&request, &mut cursor, None)
            .await
            .expect("second poll");
        assert_eq!(second.len(), 1, "only the unsent text chunk re-emits");
        assert_eq!(
            second[0].payload["sessionUpdate"], "agent_message_chunk",
            "the retry recovers the text chunk"
        );

        cursor.record(second[0].advance.clone());
        let third = engine
            .project_request_updates(&request, &mut cursor, None)
            .await
            .expect("third poll");
        assert!(third.is_empty());
    }

    async fn embedded_engine() -> (tempfile::TempDir, Arc<ProjectionEngine>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .data_path(dir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("embedded node"),
        );
        gents::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .expect("runtime schemas");
        let engine = ProjectionEngine::new(
            node,
            BoundModelContext::new(
                "GLM-5.3-NVFP4".to_string(),
                "GLM-5.3-NVFP4".to_string(),
                262_144,
            ),
        );
        (dir, Arc::new(engine))
    }

    async fn seed_projection_request(
        node: &EmbeddedNode,
        session: &str,
        request_id: &str,
    ) -> gents_protocol::row::AgentRequestRow {
        let result = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{purpose: "normal", request_id: "{}", session_id: "{}", node_did: "did:test:grok-shim", requester_did: "did:test:grok-shim", agent_id: "test", content: "projection fixture", lifecycle_state: "pending"}}) {{_docID}} }}"#, gents::graphql::escape_graphql_string(request_id), gents::graphql::escape_graphql_string(session))).await;
        ensure_no_errors(&result, "seed projection request").unwrap();
        let doc = gents_protocol::graphql::extract_mutation_doc_id(
            &json!({"data":result.data}),
            "AgentRequest",
        )
        .unwrap();
        serde_json::from_value(json!({"_docID":doc,"request_id":request_id,"node_did":"did:test:grok-shim","requester_did":"did:test:grok-shim","session_id":session})).unwrap()
    }

    async fn seed_tool_call_row(
        engine: &ProjectionEngine,
        session_id: &str,
        request_id: &str,
        request_doc_id: Option<&str>,
        tool_call_id: &str,
        tool_name: &str,
        message_sequence: i64,
    ) -> String {
        let escaped_session = gents::graphql::escape_graphql_string(session_id);
        let escaped_request = gents::graphql::escape_graphql_string(request_id);
        let escaped_id = gents::graphql::escape_graphql_string(tool_call_id);
        let escaped_name = gents::graphql::escape_graphql_string(tool_name);
        let request_doc_field = request_doc_id
            .map(|id| {
                format!(
                    r#"request_doc_id: "{}""#,
                    gents::graphql::escape_graphql_string(id)
                )
            })
            .unwrap_or_default();
        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{escaped_session}:{escaped_id}"
                    request_id: "{escaped_request}"
                    {request_doc_field}
                    session_id: "{escaped_session}"
                    node_did: "did:test:grok-shim"
                    requester_did: "did:test:grok-shim"
                    tool_call_id: "{escaped_id}"
                    tool_name: "{escaped_name}"
                    lifecycle_state: "completed"
                    message_sequence: {message_sequence}
                }}) {{ _docID }}
            }}"#
        );
        let response = engine.node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "seed tool call failed: {:?}",
            response.errors
        );
        gents_protocol::graphql::extract_mutation_doc_id(
            &json!({"data":response.data}),
            "AgentToolCall",
        )
        .unwrap()
    }

    async fn seed_canonical_tool_admission_header(
        node: &EmbeddedNode,
        request: &gents_protocol::row::AgentRequestRow,
        sequence: u32,
        calls: &[(&str, &str, &str)],
    ) {
        use gents::defra_node::QueryRequest;
        use gents::graphql::single_mutation_document;
        use gents::session::canonical_rows::{
            output_segment_create_variables, transcript_message_create_variables,
            CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        };
        use gents_protocol::output::{
            MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment,
            OutputSource, OutputWriter, PayloadRef, SegmentRun, SourceClose, StreamDeclaration,
            StreamPayload, TranscriptMessage,
        };

        let node_did = request.node_did.as_deref().unwrap();
        let session_id = request.session_id.as_deref().unwrap();
        let request_doc_id = request.doc_id.as_deref().unwrap();
        let generation = "fixture:tool-admission";
        let created_at = "2026-08-31T22:46:44Z";
        let mut blocks = Vec::new();
        for (block_index, (tool_doc_id, native_id, name)) in calls.iter().enumerate() {
            let arguments = if *name == "agent_new" {
                r#"{"target":"child-chron"}"#
            } else {
                r#"{"command":"true"}"#
            };
            let segment = OutputSegment {
                node_did: node_did.into(),
                requester_did: request.requester_did.clone(),
                session_id: session_id.into(),
                request_doc_id: request_doc_id.into(),
                source: OutputSource::Authored {
                    key: format!("tool-admission:{sequence}:{native_id}"),
                },
                writer: OutputWriter::RequestExecution {
                    execution_generation: generation.into(),
                },
                ordinal: Some(0),
                runs: vec![SegmentRun {
                    stream: 0,
                    bytes: arguments.len() as u32,
                    declaration: Some(StreamDeclaration {
                        block_index: block_index as u32,
                        part_index: 0,
                        payload: StreamPayload::ToolArguments {
                            id: (*native_id).into(),
                            call_id: None,
                            name: (*name).into(),
                        },
                    }),
                }],
                payload: arguments.into(),
                close: Some(SourceClose::Closed {
                    outcome: OutputOutcome::Complete,
                    segments: 1,
                    stream_bytes: vec![arguments.len() as u64],
                }),
                created_at: created_at.into(),
            };
            let response = node
                .execute_request(
                    QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                        .with_variables(output_segment_create_variables(&segment).unwrap()),
                )
                .await;
            assert!(
                !response.has_errors(),
                "tool args seed: {:?}",
                response.errors
            );
            let close_doc_id = single_mutation_document(&response, "create_AgentOutputSegment")
                .unwrap()
                .unwrap()["_docID"]
                .as_str()
                .unwrap()
                .to_string();
            blocks.push(MessageBlock::ToolCall {
                tool_call_doc_id: (*tool_doc_id).into(),
                id: (*native_id).into(),
                call_id: None,
                name: (*name).into(),
                arguments: PayloadRef {
                    close_doc_id,
                    stream: 0,
                },
                signature: None,
                additional_params: None,
            });
        }
        let message = TranscriptMessage {
            message_key: gents::session::sequence_message_key(
                node_did,
                session_id,
                request.requester_did.as_deref(),
                sequence,
            ),
            session_id: session_id.into(),
            node_did: node_did.into(),
            requester_did: request.requester_did.clone(),
            request_doc_id: Some(request_doc_id.into()),
            publication: MessagePublication::RequestExecution {
                execution_generation: generation.into(),
            },
            outcome: OutputOutcome::Complete,
            sequence,
            role: MessageRole::Assistant,
            native_id: None,
            blocks,
            created_at: created_at.into(),
        };
        let response = node
            .execute_request(
                QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                    .with_variables(transcript_message_create_variables(&message).unwrap()),
            )
            .await;
        assert!(
            !response.has_errors(),
            "tool header seed: {:?}",
            response.errors
        );
    }

    async fn seed_child_request_row(
        engine: &ProjectionEngine,
        parent_request_id: &str,
        parent_request_doc_id: &str,
        parent_tool_call_id: &str,
        parent_tool_call_doc_id: &str,
        child_request_id: &str,
        created_at: &str,
    ) {
        let escaped_parent = gents::graphql::escape_graphql_string(parent_request_id);
        let escaped_parent_doc = gents::graphql::escape_graphql_string(parent_request_doc_id);
        let escaped_tool_call = gents::graphql::escape_graphql_string(parent_tool_call_id);
        let escaped_tool_doc = gents::graphql::escape_graphql_string(parent_tool_call_doc_id);
        let escaped_child = gents::graphql::escape_graphql_string(child_request_id);
        let escaped_created = gents::graphql::escape_graphql_string(created_at);
        crate::commands::grok_shim::test_fixtures::seed_started_session(
            &engine.node,
            "did:test:grok-shim",
            "s-chron-child",
            Some("did:test:grok-shim"),
            "test-child",
            parent_request_doc_id,
            None,
        )
        .await;
        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(input: {{purpose: "normal", 
                    request_id: "{escaped_child}"
                    node_did: "did:test:grok-shim"
                    requester_did: "did:test:grok-shim"
                    agent_id: "test-child"
                    session_id: "s-chron-child"
                    caused_by_parent_request_id: "{escaped_parent}"
                    caused_by_parent_request_doc_id: "{escaped_parent_doc}"
                    caused_by_parent_tool_call_id: "{escaped_tool_call}"
                    caused_by_parent_tool_call_doc_id: "{escaped_tool_doc}"
                    content: "child work"
                    lifecycle_state: "processing"
                    backend_id: ""
                    execution_origin: "interactive"
                    failure_reason: ""
                    created_at: "{escaped_created}"
                    retry_count: 0
                    max_retries: 3
                }}) {{ _docID }}
            }}"#
        );
        let response = engine.node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "seed child request failed: {:?}",
            response.errors
        );
    }

    fn update_kind(event: &NovelProjectionEvent) -> String {
        event.payload["sessionUpdate"]
            .as_str()
            .expect("sessionUpdate kind")
            .to_string()
    }

    #[tokio::test]
    async fn persisted_context_hydrates_metadata_without_a_response_token_counter() {
        let (_dir, engine) = embedded_engine().await;
        let response = engine.node.execute(r#"mutation { create_AgentRequest(input: {purpose: "normal", 
            request_id: "context-owner", session_id: "context-session", node_did: "did:test:grok-shim",
            requester_did: "did:test:requester", lifecycle_state: "processing"
        }) {_docID} }"#).await;
        ensure_no_errors(&response, "context fixture owner").unwrap();
        let response = engine
            .node
            .execute(r#"{ AgentRequest(filter: {request_id: {_eq: "context-owner"}}) {_docID} }"#)
            .await;
        let doc = response.data.as_ref().unwrap()["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap();
        let request: gents_protocol::row::AgentRequestRow = serde_json::from_value(json!({
            "_docID": doc, "request_id":"context-owner", "session_id":"context-session",
            "node_did":"did:test:grok-shim", "requester_did":"did:test:requester"
        }))
        .unwrap();
        let accounting = gents_protocol::rendered_request::ContextAccounting {
            accounting_version: 1,
            turn_index: 0,
            attempt: 0,
            estimator: "fixture".into(),
            components: gents_protocol::rendered_request::ContextInputComponents {
                messages: 900,
                documents: 0,
                tool_schemas: 50,
                additional_parameters: 0,
                output_schema: 0,
            },
            estimated_input_tokens: 950,
            context_window: 10_000,
            compaction_threshold_basis_points: 8_000,
            compaction_threshold_tokens: 8_000,
            configured_max_output_tokens: Some(1_000),
            effective_max_output_tokens: Some(1_000),
            compaction_reason:
                gents_protocol::rendered_request::ContextCompactionReason::BelowThreshold,
            pre_compaction_input_tokens: None,
        };
        let encoded =
            gents::graphql::escape_graphql_string(&serde_json::to_string(&accounting).unwrap());
        let doc = gents::graphql::escape_graphql_string(doc);
        let response = engine.node.execute(&format!(r#"mutation {{ create_InferenceCall(input: {{
            call_id: "context-call", request_id: "context-owner", request_doc_id: "{doc}",
            node_did: "did:test:grok-shim", call_kind: "inference", call_seq: 1,
            queued_at: "2026-09-04T12:00:00Z", completion_tokens: 25, context_accounting_json: "{encoded}"
        }}) {{_docID}} }}"#)).await;
        ensure_no_errors(&response, "context call fixture").unwrap();
        let mut cursor = RequestCursor::new();
        engine
            .project_request_updates(&request, &mut cursor, None)
            .await
            .unwrap();
        assert_eq!(
            engine.sequencer.session_total_tokens("context-session"),
            975
        );
        let mut foreign = request.clone();
        foreign.session_id = Some("foreign-session".into());
        assert!(context::load(&engine.node, &foreign)
            .await
            .unwrap()
            .is_none());
        engine
            .project_request_updates(&request, &mut cursor, None)
            .await
            .unwrap();
        assert_eq!(
            engine.sequencer.session_total_tokens("context-session"),
            975
        );
    }

    #[tokio::test]
    async fn mixed_families_project_in_deterministic_chronology_through_the_embedded_node() {
        let (_dir, engine) = embedded_engine().await;
        let session_id = "s-chron";
        let request_id = "req-chron";

        let request = seed_projection_request(&engine.node, session_id, request_id).await;
        let parent_doc_id = request.doc_id.clone().unwrap();

        let message_key = gents::session::sequence_message_key(
            request.node_did.as_deref().unwrap(),
            request.session_id.as_deref().unwrap(),
            request.requester_did.as_deref(),
            3,
        );
        seed_canonical_assistant_message(
            &engine.node,
            &request,
            &message_key,
            3,
            "thinking",
            "answer",
        )
        .await;

        let bash_tool_doc_id = seed_tool_call_row(
            &engine,
            session_id,
            request_id,
            Some(&parent_doc_id),
            "call-z",
            "bash",
            4,
        )
        .await;
        let spawn_tool_doc_id = seed_tool_call_row(
            &engine,
            session_id,
            request_id,
            Some(&parent_doc_id),
            "call-a",
            "agent_new",
            4,
        )
        .await;
        seed_canonical_tool_admission_header(
            &engine.node,
            &request,
            4,
            &[
                (&spawn_tool_doc_id, "call-a", "agent_new"),
                (&bash_tool_doc_id, "call-z", "bash"),
            ],
        )
        .await;
        seed_child_request_row(
            &engine,
            request_id,
            &parent_doc_id,
            "call-a",
            &spawn_tool_doc_id,
            "child-chron",
            "2026-08-31T22:46:45Z",
        )
        .await;

        let mut cursor = RequestCursor::new();
        cursor.request = Some(request.clone());
        let first = engine
            .project_request_updates(&request, &mut cursor, None)
            .await
            .expect("first poll");
        let kinds: Vec<String> = first.iter().map(update_kind).collect();
        assert_eq!(
            kinds,
            vec![
                "agent_thought_chunk".to_string(),
                "agent_message_chunk".to_string(),
                "tool_call".to_string(),
                "tool_call_update".to_string(),
                "tool_call".to_string(),
                "tool_call_update".to_string(),
                "subagent_spawned".to_string(),
                "subagent_progress".to_string(),
                "available_commands_update".to_string(),
            ]
        );
        let tool_ids: Vec<&str> = first
            .iter()
            .filter(|event| event.payload["sessionUpdate"] == "tool_call")
            .map(|event| event.payload["toolCallId"].as_str().expect("toolCallId"))
            .collect();
        assert_eq!(tool_ids, vec!["call-a", "call-z"]);
        let spawned = first
            .iter()
            .find(|event| event.payload["sessionUpdate"] == "subagent_spawned")
            .expect("spawned event");
        assert_eq!(spawned.payload["subagent_id"], "s-chron-child");

        for advance in first.iter().take(3).map(|event| event.advance.clone()) {
            cursor.record(advance);
        }
        let second = engine
            .project_request_updates(&request, &mut cursor, None)
            .await
            .expect("second poll");
        let retry_kinds: Vec<String> = second.iter().map(update_kind).collect();
        assert_eq!(
            retry_kinds,
            vec![
                "tool_call_update".to_string(),
                "tool_call".to_string(),
                "tool_call_update".to_string(),
                "subagent_spawned".to_string(),
                "subagent_progress".to_string(),
                "available_commands_update".to_string(),
            ]
        );
        let retry_tool_ids: Vec<&str> = second
            .iter()
            .filter(|event| {
                matches!(
                    event.payload["sessionUpdate"].as_str(),
                    Some("tool_call") | Some("tool_call_update")
                )
            })
            .map(|event| event.payload["toolCallId"].as_str().expect("toolCallId"))
            .collect();
        assert_eq!(retry_tool_ids, vec!["call-a", "call-z", "call-z"]);

        for advance in second.iter().map(|event| event.advance.clone()) {
            cursor.record(advance);
        }
        let third = engine
            .project_request_updates(&request, &mut cursor, None)
            .await
            .expect("third poll");
        assert!(third.is_empty(), "every event is now delivered");
    }
}
