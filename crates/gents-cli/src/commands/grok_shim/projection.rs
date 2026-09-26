//! Grok shim projection engine root.
//!
//! The projection engine owns the connection-local side of the Grok shim: it
//! turns durable Gents rows (`AgentMessage`, `AgentOutputSegment`,
//! `AgentToolCall`, and the `AgentRequest` rows of caused sessions)
//! into fresh Grok pager `session/update` notification payloads and stamps the
//! per-connection event metadata (`_meta.eventId`, `_meta.promptId`,
//! `_meta.totalTokens`) those payloads require.
//!
//! The engine is deliberately bounded and request-id-scoped:
//! - every projection helper takes an explicit request id and queries only the
//!   rows that request can own, plus the sessions it caused;
//! - projection is read-only: it never replays the session, never duplicates
//!   durable materialization, and never writes a document;
//! - every interpolated GraphQL value passes through
//!   [`gents::graphql::escape_graphql_string`], and every query executes
//!   in-process through [`EmbeddedNode::execute`].
//!
//! The three leaves own the payload shapes:
//! - [`messages`]: agent/user thought and message chunks plus streaming token
//!   and context metadata;
//! - [`tools`]: tool-call lifecycle, command titles/status/content,
//!   available-command updates, and the pager-style terminal `not supported`
//!   stubs;
//! - [`caused_sessions`]: subagent spawned/progress/finished updates for the
//!   sessions a request caused, and the subagent inspection ext methods.
//!
//! Static `Task` configuration rows are never treated as runtime state and no
//! permission or terminal documents are ever fabricated here.

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

/// Wire name of the ACP `session/update` notification every projection
/// payload is wrapped in.
pub(crate) const SESSION_UPDATE_METHOD: &str = "session/update";
/// Live xAI extension rail used by Grok for subagent lifecycle events.
pub(crate) const SUBAGENT_NOTIFICATION_METHOD: &str = "x.ai/session_notification";

/// Default context window reported when the bound configuration does not
/// supply one. Mirrors the model catalog's `totalContextTokens` default scale
/// (`gents::DEFAULT_CONTEXT_WINDOW`) so a bound behavior that never pinned a
/// window still reports a truthful, bounded value instead of zero.
pub(crate) const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = gents::DEFAULT_CONTEXT_WINDOW as u64;

/// Normalize a configured context window for every Grok-facing consumer.
/// Older profiles use zero for "unspecified", but the wire catalog and token
/// projection both require the same positive effective value.
pub(crate) fn effective_context_window_tokens(configured: u64) -> u64 {
    if configured == 0 {
        DEFAULT_CONTEXT_WINDOW_TOKENS
    } else {
        configured
    }
}

/// Return a trimmed non-empty string. Projection leaves share this helper so
/// optional identity fields cannot drift subtly by row family.
pub(super) fn nonempty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Bound model/context configuration the shim was assembled with.
///
/// Model and context-window values come from the bound `AgentBehavior` and its
/// `InferenceProfile`, not from `AgentSession` (which has no model or
/// context-window fields).
#[derive(Debug, Clone)]
pub(crate) struct BoundModelContext {
    /// Grok `modelId` the pager addresses: the bound profile's `model_name`
    /// exactly. The backend id stays internal and is never projected.
    pub(crate) model_id: String,
    /// Human display name; falls back to the raw model id when the catalog
    /// has no friendlier entry.
    pub(crate) model_name: String,
    /// `totalContextTokens` reported in the session/new model catalog and
    /// used to bound `_meta.totalTokens`.
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

    /// Fall back to the catalog default when the bound profile did not pin a
    /// context window.
    pub(crate) fn effective_context_window(&self) -> u64 {
        effective_context_window_tokens(self.total_context_tokens)
    }
}

/// Connection-scoped, session-keyed projection sequencing.
///
/// One sequencer serves one registered pager connection and keys every
/// counter by session id, so two sessions on the same connection never share
/// an event counter or a token total:
/// - event ids are monotonic *per session*, formatted
///   `"{session_id}-{counter}"` and starting at 1, matching the pager's
///   `NotificationMeta` dedup contract (the pager deduplicates non-replay
///   counters by `eventId`, so a repeated id would silently drop a live
///   update);
/// - `totalTokens` is current context occupancy, ordered by persisted
///   inference dispatch; newer context may decrease after compaction.
///
/// Event ids are *reserved*, not simply allocated: a reservation commits only
/// after the notification carrying it was successfully sent, and an
/// uncommitted reservation rolls back on drop, so a failed send never
/// consumes an id. Splitting the counters out keeps the arithmetic and the
/// rollback unit-testable without an embedded node.
#[derive(Debug, Default)]
pub(crate) struct ProjectionSequencer {
    sessions: std::sync::Mutex<BTreeMap<String, SessionSequence>>,
}

/// Per-session counters: the committed event-id high-water mark and the
/// most recent observed context occupancy (not cumulative token spend).
#[derive(Debug, Default)]
struct SessionSequence {
    event_counter: u64,
    total_tokens: u64,
    context_order: Option<context::ContextOrder>,
}

/// One reserved event id.
///
/// Reserving increments the session's counter immediately (the id must be
/// stamped into the payload before it is sent), but the reservation only
/// becomes permanent on [`EventIdReservation::commit`]. Dropping an
/// uncommitted reservation rolls the counter back — and only while the
/// reservation is still the session's most recent id, so a later committed
/// id can never be un-allocated.
pub(crate) struct EventIdReservation {
    sequencer: Arc<ProjectionSequencer>,
    session_id: String,
    value: u64,
    committed: bool,
}

impl EventIdReservation {
    /// The reserved wire event id: `"{sessionId}-{counter}"`.
    pub(crate) fn event_id(&self) -> String {
        format!("{}-{}", self.session_id, self.value)
    }

    /// Keep the reserved id permanently. Called only after the notification
    /// carrying it was successfully sent.
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

    /// Reserve the next monotonic event id for `session_id`.
    ///
    /// The counter is per session and starts at 1; the reservation must be
    /// committed after the send succeeds (otherwise it rolls back on drop),
    /// so a failed send does not consume an id.
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

    /// Roll back one uncommitted reservation. Only the session's most recent
    /// id can roll back; if a later id was already committed, the failed
    /// reservation leaves a gap instead (gaps are harmless to the pager's
    /// monotonic dedup; duplicates would not be).
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

    /// The number of committed event ids for `session_id`. Test observation
    /// accessor: production send paths always commit inside the common
    /// session-update path.
    #[cfg(test)]
    pub(crate) fn event_counter(&self, session_id: &str) -> u64 {
        self.sessions
            .lock()
            .expect("grok shim sequencer lock poisoned")
            .get(session_id)
            .map(|sequence| sequence.event_counter)
            .unwrap_or(0)
    }

    /// Last known current-context occupancy for `session_id`.
    pub(crate) fn session_total_tokens(&self, session_id: &str) -> u64 {
        self.sessions
            .lock()
            .expect("grok shim sequencer lock poisoned")
            .get(session_id)
            .map(|sequence| sequence.total_tokens)
            .unwrap_or(0)
    }

    /// Replace context only from the newest persisted inference generation.
    /// New calls may lower occupancy after compaction; old background polls
    /// cannot overwrite them. Repeated observations never add token spend.
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

/// Build the `_meta` object stamped on one session/update notification.
///
/// Fields follow the pager's `NotificationMeta`: `eventId` is
/// `"{sessionId}-{counter}"`, `totalTokens` is current context occupancy,
/// and `promptId` correlates the update with its turn. `is_replay` is
/// `None` for fresh updates (the key is omitted entirely) and `Some(false)`
/// for the user echo, which carries the key explicitly.
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

/// Server-side timestamps understood by the Grok pager. `streamStartMs` is
/// also the pager's model-generation boundary key, so it must stay stable
/// within one generation and change across tool-loop generations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UpdateTimestamps {
    pub(crate) agent_timestamp_ms: Option<i64>,
    pub(crate) stream_start_ms: Option<i64>,
    pub(crate) turn_start_ms: Option<i64>,
}

/// Wrap one projected update payload in a `session/update` notification
/// envelope.
///
/// The Grok decoder expects the chunk field name `content` (not
/// `contentBlock`); the leaves own that shape and this wrapper only adds the
/// session envelope and the stamped `_meta`.
#[cfg(test)]
pub(crate) fn session_update_notification(session_id: &str, update: Value, meta: Value) -> Value {
    session_notification_for_method(SESSION_UPDATE_METHOD, session_id, update, meta)
}

/// Wrap a projected payload on its protocol rail. Standard ACP updates use
/// `session/update`; live subagent lifecycle updates use Grok's
/// `_x.ai/session_notification` wire extension rail (the similarly named
/// `x.ai/session/update` is a replay alias, not the live method).
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

// ---------------------------------------------------------------------------
// Common session-update send path
// ---------------------------------------------------------------------------

/// The connection-scoped common send path for `session/update` notifications.
///
/// One channel serves one registered pager connection and keys its send locks
/// by session id, so two sessions never serialize each other while all sends
/// for one session do. Every `session/update` family the shim emits — the
/// `session/set_mode` `current_mode_update`, the synthetic prompt
/// `user_message_chunk` echo, and the durable projected tool/subagent/message
/// updates — must go through [`SessionUpdateChannel::send`], which is what
/// makes allocation order equal successful enqueue order for every event id
/// on a session.
///
/// The allocation/enqueue invariant: the per-session send lock is held from
/// before the event id is reserved until after the notification was
/// successfully enqueued through the sender and the reservation committed.
/// The pager deduplicates non-replay counters monotonically by `eventId`, so
/// a `session-2` arriving before `session-1` would silently drop the real
/// `session-1` update as stale — uniqueness alone is not enough. A failed
/// send rolls the reservation back (the id is not consumed) and never
/// advances the caller's delivery cursor.
#[derive(Debug, Default)]
pub(crate) struct SessionUpdateChannel {
    /// The connection's projection sequencer: the shared per-session
    /// event-id and token-total counters.
    sequencer: Arc<ProjectionSequencer>,
    /// One async send lock per session id. The inner map is a short
    /// synchronous lock that only guards insertion; each session's lock is
    /// an async mutex held across the (possibly fallible) send await.
    send_locks: std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl SessionUpdateChannel {
    /// Build the channel over the connection's sequencer.
    pub(crate) fn new(sequencer: Arc<ProjectionSequencer>) -> Self {
        Self {
            sequencer,
            send_locks: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// The connection's projection sequencer.
    #[cfg(test)]
    pub(crate) fn sequencer(&self) -> &ProjectionSequencer {
        &self.sequencer
    }

    /// The per-session async send lock. Different sessions get independent
    /// locks and stay fully concurrent. The caller acquires the guard itself
    /// so the lock is held across the whole reserve → send → commit span.
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

    /// Acquire the session's send lock as an *owned* guard.
    ///
    /// `lock_owned` requires an owned `Arc` handle to the mutex and returns an
    /// [`OwnedMutexGuard`] that is a real named binding the caller holds —
    /// never a temporary guard dropped at the end of the acquiring statement.
    /// This is the exact shape the per-session ordering invariant needs: the
    /// guard stays alive from before the event id is reserved until after the
    /// notification was enqueued and the reservation committed. Returning the
    /// plain `Arc<Mutex<()>>` after `lock.lock().await;` would silently drop
    /// the temporary guard and let a racing same-session send interleave.
    async fn session_send_guard(&self, session_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        self.session_lock(session_id).lock_owned().await
    }

    /// Send one `session/update` notification through the common path.
    ///
    /// While holding the session's send lock: reads the current session
    /// token total, reserves the next event id, lets `build_notification`
    /// stamp the final notification value (the reserved event id and token
    /// total are handed in), enqueues the serialized line through
    /// `send_line`, and commits the reservation only after the send
    /// succeeded. A failed send returns the error; the uncommitted
    /// reservation rolls back on drop, so the id is not consumed and the
    /// next successful send on the session receives the immediately
    /// expected next id.
    ///
    /// Returns the serialized notification line that was delivered.
    pub(crate) async fn send(
        &self,
        session_id: &str,
        build_notification: impl FnOnce(&str, u64) -> Result<Value>,
        send_line: impl AsyncSendLine,
    ) -> Result<String> {
        self.send_with_commit(session_id, build_notification, send_line, NoCommit)
            .await
    }

    /// Send one `session/update` notification through the common path with a
    /// state-commit hook.
    ///
    /// Identical ordering and rollback semantics to [`SessionUpdateChannel::send`],
    /// plus one atomicity guarantee callers with side effects need: `commit`
    /// runs while the session's send lock is still held, immediately after
    /// the line was successfully enqueued and the event-id reservation was
    /// committed. A send failure skips `commit` entirely — so a caller like
    /// `session/set_mode` can record its mode change *inside* the hook and
    /// be certain the mode state mutates if and only if the corresponding
    /// notification was enqueued, with no window in which a concurrent
    /// same-session send can interleave between the enqueue and the state
    /// change. The hook is infallible by construction: it only ever records
    /// connection-local state, and committing the reservation before it runs
    /// is what guarantees an already-delivered event id is never reused.
    pub(crate) async fn send_with_commit(
        &self,
        session_id: &str,
        build_notification: impl FnOnce(&str, u64) -> Result<Value>,
        send_line: impl AsyncSendLine,
        commit: impl AsyncCommit,
    ) -> Result<String> {
        // Hold the session's async send lock across reserve → stamp →
        // enqueue → commit: allocation order equals enqueue order. The guard
        // is an owned guard bound here — it stays alive through the whole
        // reserve → send → commit span below (and, on the fallible paths,
        // is dropped only after the reservation rolled back).
        let _send_guard = self.session_send_guard(session_id).await;
        let reservation = ProjectionSequencer::reserve_event_id(&self.sequencer, session_id);
        let total_tokens = self.sequencer.session_total_tokens(session_id);
        let notification = build_notification(&reservation.event_id(), total_tokens)?;
        let line = serde_json::to_string(&notification)
            .context("serialize session/update notification")?;
        send_line.send_line(line.clone()).await?;
        // Commit the reservation immediately after the successful enqueue —
        // never after a further fallible operation — so the id that was
        // just delivered can never be handed to a later send. Only then run
        // the (infallible) local state hook, still inside the per-session
        // critical section so state and delivery stay coherent.
        reservation.commit();
        commit.commit().await;
        Ok(line)
    }
}

/// One infallible local state commit performed after a notification was
/// successfully enqueued, while the session's send lock is still held.
/// Implemented by callers whose session state must change exactly when the
/// corresponding notification was delivered (`session/set_mode`). The hook
/// only ever records connection-local state, so it cannot fail; the
/// reservation is committed before the hook runs, which is what guarantees
/// an already-delivered event id is never reused.
pub(crate) trait AsyncCommit: Send + Sync {
    async fn commit(&self);
}

impl<T: AsyncCommit + ?Sized> AsyncCommit for &T {
    async fn commit(&self) {
        (**self).commit().await
    }
}

/// The no-op commit used by plain [`SessionUpdateChannel::send`].
struct NoCommit;

impl AsyncCommit for NoCommit {
    async fn commit(&self) {}
}

/// One fallible enqueue of an already-serialized JSON-RPC line. Implemented
/// by the prompt sender (live outbound or test buffer); the exact commit
/// point is the successful send itself.
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

/// Connection-scoped projection engine.
///
/// One engine instance serves one registered pager connection: it holds the
/// in-process node every projection query executes against, the bound
/// model/context configuration, and the connection's projection sequencer.
pub(crate) struct ProjectionEngine {
    pub(crate) background_executions: gents::hook::BackgroundExecutionRegistry,
    node: Arc<EmbeddedNode>,
    bound: BoundModelContext,
    sequencer: Arc<ProjectionSequencer>,
    /// The connection-scoped common send path every `session/update`
    /// notification must go through (per-session send lock + reserve/send/
    /// commit), so allocation order equals enqueue order per session.
    channel: SessionUpdateChannel,
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
        }
    }

    /// The connection's common session-update send path. Every
    /// `session/update` family (set-mode updates, the prompt echo, and the
    /// durable projected updates) sends through this so per-session
    /// allocation order equals enqueue order.
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

    /// The connection's projection sequencer as a shared handle, for tests
    /// that inspect per-session counters.
    #[cfg(test)]
    pub(crate) fn sequencer_arc(&self) -> Arc<ProjectionSequencer> {
        self.sequencer.clone()
    }

    /// Poll the durable request-scoped projections and return only the
    /// *novel* events this cursor has not emitted yet, merged across
    /// families into durable transcript chronology (see step 5 below).
    ///
    /// The poll itself is read-only: it observes every projection leaf, picks
    /// the events whose durable identity is new or changed relative to this
    /// cursor, and returns each together with the cursor advance that
    /// records it. **The cursor is not mutated here** — the caller records
    /// each advance only after the corresponding line was successfully sent,
    /// so a send failure never marks a novel event as delivered. Event ids
    /// are likewise *reserved* by the caller (see
    /// [`ProjectionSequencer::reserve_event_id`]) and committed only after a
    /// successful send.
    ///
    /// Ordering and identity rules:
    /// - live tails: protocol-owned canonical output prefixes
    ///   (the streaming snapshot of the *current* assistant segment) plan
    ///   deltas against a shadow copy of the live cursors, so several
    ///   candidates in one poll each see the preceding planned advances and
    ///   a failed send re-plans the identical candidate next poll. Exact
    ///   source identity separates generations; within a source only newly
    ///   validated suffix bytes are emitted.
    /// - tool calls: the first observation of a `tool_call` base emits the
    ///   full tracker registration; a later change to the tracked fields
    ///   (`title`/`kind`/`status`/`content`/`rawInput`/`rawOutput`/`meta`)
    ///   emits a `tool_call_update` carrying exactly the changed fields. The
    ///   terminal status has a dedicated status-only update whose delivery
    ///   is tracked separately from content refinements.
    ///   `available_commands_update` emits once per distinct visible tool
    ///   list.
    /// - subagents: one event per distinct payload per
    ///   `<sessionUpdate kind>:<subagentId>`; a still-running child's
    ///   `durationMs` is 0 (the elapsed computation needs a terminal bound),
    ///   so running progress payloads are stable across polls.
    /// - durable messages: each `AgentMessage`-derived chunk keeps a
    ///   delivered-length state keyed by `(message_key, update kind,
    ///   ordinal)`; an upserted/grown row re-projects and emits only the
    ///   newly proven suffix, never "seen forever" after its first
    ///   observation. The durable view reconciles against the live view: a
    ///   durable final row bound by exact source/header provenance emits
    ///   only the bytes the live cursor has not already sent of the same
    ///   logical segment, and live bytes already covering a row suppress its
    ///   replay. The synthetic `user_message_chunk` echo of the current
    ///   prompt's user row is skipped — the turn already echoed the prompt
    ///   blocks directly.
    ///
    /// Context metadata comes from the newest physically owned inference
    /// accounting observation, not generated-token totals. Re-observation
    /// after a failed send is idempotent; older requests cannot replace it.
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
                && request
                    .agent_did
                    .as_deref()
                    .is_some_and(|id| !id.is_empty()),
            "projection requires actual scoped physical request"
        );
        // Each family projects independently (one bounded query set per
        // leaf), then the novel events merge into one chronology below.
        let mut merged: Vec<MergedEvent> = Vec::new();

        // 1. Messages leaf query (live tail + durable rows). The live tails
        //    plan against a shadow copy of the live cursors; the durable
        //    rows reconcile against that planned state below.
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
        // 4. Tools (lifecycle of the request's tool calls).
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

        // 2. Subagents (sessions this request caused).
        let subagents = caused_sessions::project_caused_sessions(
            &self.node,
            request,
            parent_prompt_id,
            self.bound.effective_context_window(),
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
                family_rank: FAMILY_RANK_SUBAGENT,
                family_ordinal: merged
                    .iter()
                    .filter(|item| item.family_rank == FAMILY_RANK_SUBAGENT)
                    .count(),
            });
        }

        // 5. Durable messages (assistant transcript chunks), reconciled
        //    against the live view planned above. The user echo of the
        //    current prompt is skipped: the turn already sent it directly.
        //    The durable pass plans against the same shadow state the live
        //    pass advanced (plus a shadow of the durable chunk states), so
        //    live and durable observations of the same logical segment never
        //    duplicate and a failed send replans the identical candidates.
        //    Intermediate rows (without the materialization pointer) are
        //    reconsidered for upsert growth in transcript order.
        let mut durable_trailing = Vec::new();
        {
            let mut planned_durable = cursor.durable_chunks.clone();
            // Aggregate each durable row/rail before applying live evidence:
            // one live segment spans all text blocks of that rail, so
            // comparing evidence independently to each block would duplicate
            // multi-block rows.
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

            // Exact canonical source/header bindings were applied above;
            // durable chunks without that physical provenance remain novel.
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
                    // Keep the exact durable wakeup echo, but tag it as
                    // runtime input using Grok's native hidden-echo metadata.
                    // Lifecycle events surface the completion in the UI.
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
                // Suppress live bytes only when a canonical payload reference
                // proves this exact source closed into this exact durable row.
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
                    // A replacement/shrink/divergence is not growth. Never
                    // slice an unrelated UTF-8 value at a stale byte offset;
                    // project the new authoritative value in full.
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
                // UTF-8 safety: `sent_len` is either zero, a previously
                // observed chunk-text length of this same row, or a
                // live-prefix length of the same logical segment's bytes —
                // all char boundaries of `text`.
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

        // 6. Cross-family merge: emit in durable chronology order, never
        // family-batched. The primary key is the durable transcript position
        // each family shares (tool `message_sequence`, message `sequence`,
        // and the `message_sequence` of the call that caused a subagent session
        // all allocate from
        // the same session transcript sequence space), so a client replaying
        // the stream observes tool calls, subagent lifecycles, and message
        // chunks in the order the transcript recorded them. Ties break by
        // family rank: message chunks of an assistant turn precede the tool
        // call that turn issued (thought-before-text precedes the call), and
        // a `subagent_spawned` follows its causing tool call. Within a family,
        // equal positions break by the durable stable identity each family's
        // decoded rows were sorted by (the tool call's stable id, the caused
        // session order), so the merged wire
        // order is a pure function of the durable rows and never of query
        // iteration order. Positionless events
        // (`available_commands_update`, rows without a sequence, and
        // subagents without a causing tool row) sort after every positioned event
        // of their family, preserving each family's own emission order.
        merged.sort_by(|a, b| family_sort_key(a).cmp(&family_sort_key(b)));
        let events: Vec<NovelProjectionEvent> = merged.into_iter().map(|item| item.event).collect();
        let mut trailing_advances = durable_trailing;
        if let Some(sequence) = messages.message_sequence_high_water {
            trailing_advances.push(CursorAdvance::MessageHighWater { sequence });
        }
        // The final shadow rail states include every no-byte commit and every
        // evidence binding. They are the batch suffix and commit only after
        // all wire events succeed.
        Ok(ProjectionBatch {
            events,
            trailing_advances,
        })
    }
}

/// Family ranks for the cross-family merge at equal chronology. Lower rank
/// emits first: message chunks (reasoning precedes the assistant turn's tool
/// call), then the tool call, then the session that tool call caused.
const FAMILY_RANK_MESSAGE: u8 = 0;
const FAMILY_RANK_TOOL: u8 = 1;
const FAMILY_RANK_SUBAGENT: u8 = 2;

/// One novel event tagged with its durable chronology key and merge tiebreak
/// data. Internal to [`ProjectionEngine::project_request_updates`].
struct MergedEvent {
    event: NovelProjectionEvent,
    /// Durable transcript position (`None` = positionless).
    chronology: Option<i64>,
    /// Family rank for ties at the same chronology.
    family_rank: u8,
    /// Zero-based emission ordinal within this poll's family stream, keeping
    /// each family's own order for positionless tails.
    family_ordinal: usize,
}

/// The full sort key of one merged event: `(position, family rank,
/// family ordinal)`. Positionless events sort last within their family by
/// using a sentinel position of `i64::MAX`.
fn family_sort_key(event: &MergedEvent) -> (i64, u8, usize) {
    (
        event.chronology.unwrap_or(i64::MAX),
        event.family_rank,
        event.family_ordinal,
    )
}

/// One novel projection event: the update payload to send plus the cursor
/// advance that records its durable identity once the send succeeds.
#[derive(Debug, Clone)]
pub(crate) struct NovelProjectionEvent {
    /// JSON-RPC notification method for this event's protocol family.
    pub(crate) method: &'static str,
    /// The `session/update` payload (`sessionUpdate` object) to wrap and
    /// send.
    pub(crate) payload: Value,
    /// Stable logical model-generation identity and its best durable start
    /// candidate. The turn sender resolves this into one request-local,
    /// strictly increasing `streamStartMs` and reuses it on every later
    /// chunk/retry of the same segment.
    pub(crate) timing: Option<ProjectionEventTiming>,
    /// The advance that records this event as delivered once it is sent.
    pub(crate) advance: CursorAdvance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionEventTiming {
    pub(crate) segment_key: String,
    pub(crate) stream_start_candidate_ms: Option<i64>,
    pub(crate) agent_timestamp_candidate_ms: Option<i64>,
}

/// One fully planned projection poll.
///
/// Byte-carrying advances travel with their outbound event. State changes
/// which intentionally carry no wire bytes (for example a tail-reset
/// snapshot) are held in `trailing_advances` and committed only after every
/// event in this batch was sent successfully. This keeps an unsent earlier
/// byte event from being leapfrogged by a later reset/no-op observation.
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

/// The recorded identity of one novel projection event. Recorded only after
/// the corresponding notification line was successfully sent.
#[derive(Debug, Clone)]
pub(crate) enum CursorAdvance {
    /// Apply several infallible cursor transitions in order at one delivery
    /// commit point.
    Many(Vec<CursorAdvance>),
    /// The full base payload of a tool call was observed (first time or
    /// changed tracked fields).
    ToolBase {
        tool_call_id: String,
        payload: Value,
    },
    /// A terminal same-id tool update was delivered (needed separately from
    /// the base registration so a first-observed-terminal task clears the
    /// pager's foreground wait).
    ToolTerminal {
        tool_call_id: String,
        status: String,
    },
    /// A distinct visible tool list was observed.
    Commands { fingerprint: u64 },
    /// A distinct subagent payload was observed for its key.
    Subagent { key: String, fingerprint: u64 },
    /// One native background task lifecycle notification was delivered.
    BackgroundTask { key: String },
    BackgroundOutput {
        key: String,
        fingerprint: u64,
        output_start: Option<u64>,
    },
    ChildOutput {
        key: String,
        receipt: child_output::OutputReceipt,
    },
    /// A canonical live prefix delta was planned and sent.
    LiveContent { plan: LiveSegmentPlan },
    /// Same as [`CursorAdvance::LiveContent`] for the reasoning tail.
    LiveReasoning { plan: LiveSegmentPlan },
    /// A durable message chunk's exact delivered text advanced after send.
    DurableChunk {
        message_key: String,
        sent_text: String,
    },
    /// Inclusive durable transcript query cursor. This advances only after
    /// the complete projection batch succeeds, so a leaf/send failure re-reads
    /// every still-undelivered row.
    MessageHighWater { sequence: i64 },
}

/// The post-send state of one live tail cursor, carried inside a
/// [`CursorAdvance`] so the send-success `record` path is the only mutator of
/// the real cursor's *delivered* state.
#[derive(Clone, Debug, Default)]
pub(crate) struct LiveSegmentPlan {
    /// Exact serialized `OutputSource` identity.
    segment_key: Option<String>,
    /// The observed tail snapshot after this send.
    observed: String,
    /// How many bytes of the current segment's logical stream were
    /// successfully sent (including this delta).
    sent_len: usize,
    /// The exact bytes of the current segment already sent, retained so the
    /// durable reconciliation can prove a live prefix covers a durable row's
    /// start even after the observed window rolls or the tail resets.
    sent_bytes: String,
    /// Delivered closed segments awaiting (or carrying) an exact durable-row
    /// binding. This queue is never arbitrarily capped: dropping an older
    /// entry could make a later durable row duplicate bytes already sent.
    closed_evidence: Vec<ClosedEvidence>,
}

/// A request-local canonical live cursor for one logical byte stream.
/// Exact `OutputSource` identity separates segments; within one source the
/// protocol owner exposes only a validated contiguous prefix.
///
/// - `observed`: the most recent validated contiguous prefix;
/// - `sent_len` / `sent_bytes`: how many bytes of the current segment's
///   logical stream have been *successfully sent*, and their exact bytes;
///
/// ## Append-only / no-loss policy (documented contract)
///
/// ACP chunks are append-only: bytes that were already sent can never be
/// retracted. A divergence — the freshly observed tail no longer starts with
/// the previously observed snapshot (a TurnRetracted, a retracted turn, or a
/// racing replacement source) — therefore never slices into the
/// sent prefix and never pretends already-sent bytes can be taken back.
/// Instead the divergence *closes* the current segment and the whole freshly
/// observed snapshot opens a new segment: the un-sent remainder of the old
/// segment is deliberately dropped (the runtime retracted it), while the new
/// observation is streamed in full — no part of what the durable row now
/// shows is ever lost.
#[derive(Clone, Debug, Default)]
pub(crate) struct LiveSegmentCursor {
    /// See [`LiveSegmentPlan::segment_key`].
    segment_key: Option<String>,
    /// The most recently observed snapshot of the live tail.
    observed: String,
    /// How many bytes of the current segment's logical stream were already
    /// successfully sent.
    sent_len: usize,
    /// The exact already-sent bytes of the current segment (evidence for
    /// live/durable reconciliation after a window roll or tail reset).
    sent_bytes: String,
    /// Ordered delivered evidence for every closed logical segment.
    closed_evidence: Vec<ClosedEvidence>,
}

/// The request-local pair of live tail cursors: reasoning and content.
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

/// Request-local dedup cursor for one live turn's projection poll.
///
/// One cursor serves exactly one (session id, prompt id) turn: it is created
/// when the turn's watch loop starts and dropped when the turn resolves, so
/// it is never shared across prompts and never outlives its request. It
/// tracks the last-sent durable identity of every projection event family:
///
/// - tool calls: the last-sent base payload per `toolCallId`; a later
///   change to a tracked field emits a `tool_call_update` with exactly the
///   changed fields;
/// - available commands: the last-sent tool-list fingerprint;
/// - subagents: the last-sent payload fingerprint per
///   `<sessionUpdate kind>:<subagentId>`;
/// - live tails: one [`LiveSegmentCursor`] per stream (content, reasoning);
/// - durable message chunks: a delivered-length state per
///   `(message_key, update kind, ordinal)` chunk key, so an upserted/grown
///   row re-projects and emits only the newly proven suffix.
///
/// The poll computes novel events against *shadow copies* of the live and
/// durable chunk states (never mutating the cursor); each shadow advance is
/// carried inside its [`CursorAdvance`] and the caller records it only after
/// the corresponding send succeeded, so a send failure replays the identical
/// candidates on the next poll instead of dropping or duplicating them.
#[derive(Debug, Default)]
pub(crate) struct RequestCursor {
    /// Actual immutable request identity selected by a signed receipt or
    /// scoped bridge read. Logical labels never rebind this cursor.
    pub(crate) request: Option<gents_protocol::row::AgentRequestRow>,
    /// Last-sent base payload per tool call id.
    tool_bases: BTreeMap<String, Value>,
    /// Last delivered terminal status update per tool call.
    tool_terminal_states: BTreeMap<String, String>,
    /// Last-sent visible tool list fingerprint.
    commands_state: Option<u64>,
    /// Last-sent payload fingerprint per subagent key.
    subagent_states: BTreeMap<String, u64>,
    /// Delivery receipts only; process lifecycle remains in AgentToolCall.
    background_task_events: std::collections::BTreeSet<String>,
    background_outputs: BTreeMap<String, u64>,
    child_outputs: BTreeMap<String, child_output::OutputReceipt>,
    /// The committed (send-success) state of the live tail cursors.
    live_cursors: LiveCursorPair,
    /// The committed (send-success) delivered length per durable chunk key.
    /// The durable pass plans against a per-poll shadow of this map; an
    /// advance is promoted into it only through `record` after the
    /// corresponding send succeeded, so a failed send replans the identical
    /// durable candidate on the next poll.
    durable_chunks: BTreeMap<String, DurableChunkState>,
    /// Last transcript sequence whose complete projection batch succeeded.
    /// Queries include this row because the current assistant row may grow.
    message_sequence_high_water: Option<i64>,
    /// Timestamp evidence is observation state, not delivery state. Retain
    /// it across incremental pages so a growing current assistant row keeps
    /// the same start derived from its preceding tool-result/input row.
    response_started_at_ms: Option<i64>,
    response_ended_at_ms: Option<i64>,
    message_timestamps: BTreeMap<i64, (String, Option<i64>)>,
}

/// The rail one row's live evidence streamed on.
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

    /// The novel event for one tool call's base payload: the full
    /// `tool_call` registration on first observation, or a
    /// `tool_call_update` carrying exactly the tracked fields that changed
    /// since the last sent base. `None` means nothing tracked changed.
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
                // Lifecycle completion has its own status-only event and
                // send-success cursor. A content refinement must not consume
                // that event if delivery stops between the two sends.
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

    /// Whether the visible tool list is novel.
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

    /// Whether the subagent payload is novel for its key.
    pub(super) fn subagent_spawn_was_delivered(&self, child_session_id: &str) -> bool {
        self.subagent_states
            .contains_key(&format!("subagent_spawned:{child_session_id}"))
    }

    fn subagent_changed(&mut self, key: &str, fingerprint: u64) -> Option<CursorAdvance> {
        if self.subagent_states.get(key) == Some(&fingerprint) {
            return None;
        }
        Some(CursorAdvance::Subagent {
            key: key.to_string(),
            fingerprint,
        })
    }

    fn background_task_novel(&self, key: &str) -> Option<CursorAdvance> {
        (!self.background_task_events.contains(key)).then(|| CursorAdvance::BackgroundTask {
            key: key.to_string(),
        })
    }

    /// Record one delivered event after its send succeeded.
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
            CursorAdvance::Subagent { key, fingerprint } => {
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

/// A per durable chunk delivery state: how much of the chunk's logical
/// text has already been *successfully sent*.
///
/// A durable `AgentMessage` row can appear before the request
/// terminalizes and be upserted (same `message_key`/sequence, growing
/// content), so the cursor must never mark a row "seen forever" after its
/// first observation. Instead each chunk keeps the exact delivered length
/// of its text and a re-projection emits only the newly proven suffix.
#[derive(Clone, Debug, Default)]
struct DurableChunkState {
    /// Exact logical text of this chunk already successfully sent. Prefix
    /// equality, not length alone, proves a later observation is growth.
    sent_text: String,
}

/// The tool-call fields the live poll tracks for diffs. A change to any of
/// these emits a `tool_call_update` carrying exactly the changed fields; a
/// first observation emits the full `tool_call` registration.
const TRACKED_TOOL_FIELDS: [&str; 7] = [
    "title",
    "kind",
    "status",
    "content",
    "rawInput",
    "rawOutput",
    "_meta",
];

/// The tracked tool-call fields that differ between the last-sent base and
/// the freshly observed payload, as a JSON object for a `tool_call_update`.
/// `None` when nothing tracked changed.
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

/// A stable fingerprint of one projection payload: order-insensitive over
/// JSON object keys (a serialized `serde_json::Value` iterates object keys
/// in sorted order, so two payloads that differ only in key insertion order
/// hash identically) while remaining sensitive to every value and array
/// order.
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
            // serde_json preserves insertion order, so iterate sorted to
            // make the fingerprint insensitive to key order.
            for (key, value) in fields.iter().collect::<BTreeMap<_, _>>() {
                key.hash(hasher);
                hash_value(hasher, value);
            }
        }
    }
}

/// Project the exact principal's behavior → profile → backend selection.
/// Model identity is the provider model name; backend identity stays internal.
pub(crate) async fn resolve_bound_model_context(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
) -> Result<BoundModelContext> {
    use crate::commands::inference_binding::{load_bound_context_window, load_bound_profile};
    let profile = load_bound_profile(node, agent_did, behavior_id).await?;
    let context_window = load_bound_context_window(node, agent_did, behavior_id).await?;
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    #[test]
    fn terminal_tail_uses_persisted_end_not_replay_arrival() {
        let mut cursor = RequestCursor::default();
        cursor.response_started_at_ms = Some(1000);
        cursor.response_ended_at_ms = Some(2500);
        let tail = cursor.timing_for_segment("tail".into(), None);
        assert_eq!(tail.stream_start_candidate_ms, Some(1000));
        assert_eq!(tail.agent_timestamp_candidate_ms, Some(2500));
        cursor
            .message_timestamps
            .insert(1, ("message".into(), Some(1500)));
        let durable = cursor.timing_for_segment("durable".into(), Some(1));
        assert_eq!(durable.agent_timestamp_candidate_ms, Some(1500));
        cursor.response_ended_at_ms = None;
        assert_eq!(
            cursor
                .timing_for_segment("live".into(), None)
                .agent_timestamp_candidate_ms,
            None
        );
    }

    #[test]
    fn background_task_receipts_advance_only_after_delivery() {
        let mut cursor = RequestCursor::default();
        let started = "task_backgrounded:call";
        let done = "task_completed:call";
        assert!(cursor.background_task_novel(started).is_some());
        // A planned but failed send remains retryable.
        let retry = cursor.background_task_novel(started).unwrap();
        cursor.record(retry);
        assert!(cursor.background_task_novel(started).is_none());
        let completion = cursor.background_task_novel(done).unwrap();
        assert!(cursor.background_task_novel(done).is_some());
        cursor.record(completion);
        assert!(cursor.background_task_novel(done).is_none());
        assert!(cursor
            .background_task_novel("task_backgrounded:other")
            .is_some());
    }

    /// A deterministic sender that records wire-enqueue order and can delay
    /// or fail sends: exactly the shape a closed/failing live outbound has.
    struct RecordingSender {
        lines: StdMutex<Vec<String>>,
        first_send_delay: tokio::sync::Notify,
        delay_armed: AtomicBool,
        fail_all: AtomicBool,
        /// Completed (not merely attempted) sends. Only incremented after a
        /// send finished enqueueing or failing.
        sends: AtomicUsize,
        /// Sends that have parked inside their delay. Incremented *before*
        /// the send awaits the release notification, so a test can wait on
        /// it without deadlocking against the parked send itself.
        parked: AtomicUsize,
    }

    impl RecordingSender {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                lines: StdMutex::new(Vec::new()),
                first_send_delay: tokio::sync::Notify::new(),
                delay_armed: AtomicBool::new(false),
                fail_all: AtomicBool::new(false),
                sends: AtomicUsize::new(0),
                parked: AtomicUsize::new(0),
            })
        }

        fn recorded_lines(&self) -> Vec<String> {
            self.lines.lock().expect("lines").clone()
        }
    }

    impl AsyncSendLine for RecordingSender {
        async fn send_line(&self, line: String) -> Result<()> {
            if self.delay_armed.swap(false, Ordering::SeqCst) {
                // The first send parks until the test releases it, so a
                // racing second send deterministically arrives while the
                // first still holds the session's send lock. `parked` is
                // counted before the await so the test has an observable
                // "has parked" signal that the parked send itself cannot
                // miss.
                self.parked.fetch_add(1, Ordering::SeqCst);
                self.first_send_delay.notified().await;
            }
            self.sends.fetch_add(1, Ordering::SeqCst);
            if self.fail_all.load(Ordering::SeqCst) {
                anyhow::bail!("sender closed");
            }
            self.lines.lock().expect("lines").push(line);
            Ok(())
        }
    }

    /// A no-op payload builder: the notification body is irrelevant to the
    /// ordering assertions; the `_meta.eventId` is what the tests read.
    fn plain_update(event_id: &str, _total_tokens: u64) -> Result<Value> {
        Ok(json!({ "eventId": event_id }))
    }

    /// The pager's `NotificationMeta` read: `_meta.eventId` of one recorded
    /// line.
    fn recorded_event_ids(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                serde_json::from_str::<Value>(line)
                    .expect("recorded line is JSON")
                    .get("eventId")
                    .and_then(Value::as_str)
                    .expect("eventId")
                    .to_string()
            })
            .collect()
    }

    /// Gate 1/3: a deliberately delayed first same-session send and a
    /// racing second send. The wire enqueue order must still be the strictly
    /// increasing event-id allocation order — the second send cannot
    /// overtake the first even though the first parked inside its enqueue.
    #[tokio::test]
    async fn a_delayed_first_send_is_not_overtaken_by_a_racing_second_send() {
        let sequencer = Arc::new(ProjectionSequencer::new());
        let channel = Arc::new(SessionUpdateChannel::new(sequencer.clone()));
        let sender = RecordingSender::new();

        // Arm the delay, start the first send, and let it acquire the
        // session lock and park inside its enqueue.
        sender.delay_armed.store(true, Ordering::SeqCst);
        let first_sender = sender.clone();
        let first_channel = channel.clone();
        let first = tokio::spawn(async move {
            first_channel
                .send("s", plain_update, first_sender)
                .await
                .expect("first send")
        });
        // Yield until the first send has actually parked inside its enqueue;
        // this makes the race deterministic. Waiting on `parked` (not
        // `sends`) is what makes the wait sound: the parked send increments
        // it before awaiting, so the signal can never be lost to the very
        // delay the test is about to release.
        while sender.parked.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        // The racing second send must block on the session's send lock
        // until the first released it — it cannot enqueue before the first.
        let second_sender = sender.clone();
        let second_channel = channel.clone();
        let second = tokio::spawn(async move {
            second_channel
                .send("s", plain_update, second_sender)
                .await
                .expect("second send")
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Release the delayed first send; both sends now complete.
        sender.first_send_delay.notify_one();
        let (first_line, second_line) = tokio::join!(first, second);
        let ids = recorded_event_ids(&[first_line.unwrap(), second_line.unwrap()]);
        assert_eq!(
            ids,
            vec!["s-1".to_string(), "s-2".to_string()],
            "same-session wire enqueue order must equal allocation order"
        );
        assert_eq!(sequencer.event_counter("s"), 2);
    }

    /// Gate 3: two sessions both start at event id 1 and are *not*
    /// serialized behind one another — a parked send on session A does not
    /// block a concurrent send on session B.
    #[tokio::test]
    async fn two_sessions_start_at_one_and_stay_independently_concurrent() {
        let sequencer = Arc::new(ProjectionSequencer::new());
        let channel = Arc::new(SessionUpdateChannel::new(sequencer.clone()));
        let sender = RecordingSender::new();

        // Park session A's first send inside its enqueue.
        sender.delay_armed.store(true, Ordering::SeqCst);
        let sender_a = sender.clone();
        let parked_channel = channel.clone();
        let parked = tokio::spawn(async move {
            parked_channel
                .send("session-a", plain_update, sender_a)
                .await
                .expect("parked send")
        });
        // Wait until session A's first send has actually parked inside its
        // enqueue (see the `parked` counter rationale above).
        while sender.parked.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        // Session B sends concurrently and must complete *without waiting*
        // for session A's parked send.
        let sender_b = sender.clone();
        let concurrent_channel = channel.clone();
        let concurrent_handle = tokio::spawn(async move {
            concurrent_channel
                .send("session-b", plain_update, sender_b)
                .await
                .expect("concurrent send")
        });
        let concurrent = tokio::time::timeout(std::time::Duration::from_secs(5), concurrent_handle)
            .await
            .expect("session B must not block behind session A's parked send")
            .expect("join");

        // Both sessions started at 1: per-session counters, never shared.
        // Release session A's parked send and collect its line: the
        // assertion that matters already passed above — session B completed
        // while A was still parked.
        sender.first_send_delay.notify_one();
        let parked_line = parked.await.expect("join");
        assert_eq!(
            recorded_event_ids(&[parked_line, concurrent]),
            vec!["session-a-1".to_string(), "session-b-1".to_string()]
        );
        assert_eq!(sequencer.event_counter("session-a"), 1);
        assert_eq!(sequencer.event_counter("session-b"), 1);
    }

    /// Gate 2: a deterministic failing sender. A failed send consumes no
    /// event id, and the following successful send receives the expected
    /// next id.
    #[tokio::test]
    async fn a_failed_send_consumes_no_event_id_and_the_next_send_gets_the_expected_id() {
        let sequencer = Arc::new(ProjectionSequencer::new());
        let channel = SessionUpdateChannel::new(sequencer.clone());
        let sender = RecordingSender::new();

        // The sender fails: the send returns an error and nothing is
        // enqueued.
        sender.fail_all.store(true, Ordering::SeqCst);
        let failure = channel
            .send("s", plain_update, sender.clone())
            .await
            .expect_err("the closed sender must fail the send");
        assert!(failure.to_string().contains("sender closed"));
        assert!(sender.recorded_lines().is_empty());
        assert_eq!(
            sequencer.event_counter("s"),
            0,
            "a failed send must consume no event id"
        );

        // Recover the sender: the next successful send receives the
        // immediately expected next id — the failed reservation rolled back.
        sender.fail_all.store(false, Ordering::SeqCst);
        let recovered = channel
            .send("s", plain_update, sender.clone())
            .await
            .expect("the recovered send must succeed");
        assert_eq!(
            recorded_event_ids(&[recovered]),
            vec!["s-1".to_string()],
            "the next successful send must receive the expected next id"
        );
        assert_eq!(sequencer.event_counter("s"), 1);
    }

    /// Gate 2 (state-commit hook): the commit hook runs only after a
    /// successful enqueue — a failed send leaves the recorded state
    /// untouched and the counter at zero. The hook is infallible (it only
    /// records connection-local state), and the reservation is committed
    /// before the hook runs, so an already-delivered id is never reused.
    #[tokio::test]
    async fn a_failed_send_skips_the_state_commit_hook() {
        struct RecordingCommit {
            committed: AtomicUsize,
        }
        impl AsyncCommit for RecordingCommit {
            async fn commit(&self) {
                self.committed.fetch_add(1, Ordering::SeqCst);
            }
        }

        let sequencer = Arc::new(ProjectionSequencer::new());
        let channel = SessionUpdateChannel::new(sequencer.clone());
        let sender = RecordingSender::new();
        let commit = RecordingCommit {
            committed: AtomicUsize::new(0),
        };

        sender.fail_all.store(true, Ordering::SeqCst);
        channel
            .send_with_commit("s", plain_update, sender.clone(), &commit)
            .await
            .expect_err("the closed sender must fail the send");
        assert_eq!(
            commit.committed.load(Ordering::SeqCst),
            0,
            "a failed send must not commit state"
        );
        assert_eq!(sequencer.event_counter("s"), 0);

        sender.fail_all.store(false, Ordering::SeqCst);
        channel
            .send_with_commit("s", plain_update, sender, &commit)
            .await
            .expect("the recovered send must succeed");
        assert_eq!(
            commit.committed.load(Ordering::SeqCst),
            1,
            "a successful send commits the state exactly once"
        );
        assert_eq!(sequencer.event_counter("s"), 1);
    }

    /// Context observation is independent of transport delivery. A failed
    /// send rolls back its event ID; re-observing the same inference sample
    /// neither adds usage nor changes context, and the retry stamps it once.
    #[tokio::test]
    async fn a_failed_send_rolls_back_the_id_but_never_double_counts_tokens() {
        let sequencer = Arc::new(ProjectionSequencer::new());
        let channel = SessionUpdateChannel::new(sequencer.clone());
        let sender = RecordingSender::new();

        // The projection pass observed 100 tokens for the request and
        // applied the delta to the session total at poll time.
        sequencer.observe_context("s", test_context(1, 100));
        assert_eq!(sequencer.session_total_tokens("s"), 100);

        // The send fails: the id rolls back, nothing is enqueued.
        sender.fail_all.store(true, Ordering::SeqCst);
        channel
            .send("s", plain_update, sender.clone())
            .await
            .expect_err("the closed sender must fail the send");
        assert_eq!(sequencer.event_counter("s"), 0);

        // The next poll re-observes the same generation and value.
        sequencer.observe_context("s", test_context(1, 100));
        assert_eq!(sequencer.session_total_tokens("s"), 100);

        // The recovery send stamps the recorded cumulative total (100) with
        // the rolled-back id, in one coherent notification.
        sender.fail_all.store(false, Ordering::SeqCst);
        let recovered = channel
            .send("s", plain_update, sender)
            .await
            .expect("the recovered send must succeed");
        let recovered: Value = serde_json::from_str(&recovered).expect("line is JSON");
        assert_eq!(recovered["eventId"], "s-1");
        assert_eq!(
            sequencer.session_total_tokens("s"),
            100,
            "the recovery send must not re-add the observed delta"
        );
        assert_eq!(sequencer.event_counter("s"), 1);
    }

    #[test]
    fn event_ids_are_session_keyed_and_monotonic() {
        let sequencer = Arc::new(ProjectionSequencer::new());
        let first = ProjectionSequencer::reserve_event_id(&sequencer, "session-1");
        assert_eq!(first.event_id(), "session-1-1");
        let second = ProjectionSequencer::reserve_event_id(&sequencer, "session-1");
        assert_eq!(second.event_id(), "session-1-2");
        second.commit();
        first.commit();
        // A different session starts at 1: counters are per session, never
        // connection-wide.
        let third = ProjectionSequencer::reserve_event_id(&sequencer, "session-2");
        assert_eq!(third.event_id(), "session-2-1");
        third.commit();
        assert_eq!(sequencer.event_counter("session-1"), 2);
        assert_eq!(sequencer.event_counter("session-2"), 1);
    }

    #[test]
    fn a_fresh_sequencer_allocates_no_ids_or_tokens() {
        let sequencer = ProjectionSequencer::new();
        assert_eq!(sequencer.event_counter("s"), 0);
        assert_eq!(sequencer.session_total_tokens("s"), 0);
    }

    #[test]
    fn a_failed_send_rolls_back_the_uncommitted_event_id() {
        let sequencer = Arc::new(ProjectionSequencer::new());
        // Simulate a failed send: reserve, never commit, drop.
        {
            let reservation = ProjectionSequencer::reserve_event_id(&sequencer, "s");
            assert_eq!(reservation.event_id(), "s-1");
            // Dropped without commit: the send failed.
        }
        assert_eq!(sequencer.event_counter("s"), 0, "the id must roll back");
        // The next successful send reuses the rolled-back id.
        let next = ProjectionSequencer::reserve_event_id(&sequencer, "s");
        assert_eq!(next.event_id(), "s-1");
        next.commit();
        assert_eq!(sequencer.event_counter("s"), 1);
    }

    #[test]
    fn an_uncommitted_reservation_leaves_a_gap_when_a_later_id_committed() {
        let sequencer = Arc::new(ProjectionSequencer::new());
        let first = ProjectionSequencer::reserve_event_id(&sequencer, "s");
        let second = ProjectionSequencer::reserve_event_id(&sequencer, "s");
        // The later id sends successfully first; the earlier reservation
        // then fails: it cannot un-allocate the committed id, so it leaves a
        // gap instead.
        second.commit();
        drop(first);
        assert_eq!(sequencer.event_counter("s"), 2);
        let third = ProjectionSequencer::reserve_event_id(&sequencer, "s");
        assert_eq!(third.event_id(), "s-3");
        third.commit();
        assert_eq!(sequencer.event_counter("s"), 3);
    }

    fn test_context(generation: i64, used: u64) -> context::ContextSample {
        context::ContextSample {
            order: (
                chrono::DateTime::from_timestamp(1_700_000_000 + generation, 0).unwrap(),
                generation,
                format!("call-{generation}"),
            ),
            used,
        }
    }

    #[test]
    fn context_observations_replace_spend_and_reject_old_requests() {
        let sequencer = ProjectionSequencer::new();
        sequencer.observe_context("s", test_context(1, 900));
        sequencer.observe_context("s", test_context(1, 900));
        assert_eq!(sequencer.session_total_tokens("s"), 900);
        sequencer.observe_context("s", test_context(2, 300));
        assert_eq!(
            sequencer.session_total_tokens("s"),
            300,
            "compaction can reduce current context"
        );
        sequencer.observe_context("s", test_context(1, 950));
        assert_eq!(
            sequencer.session_total_tokens("s"),
            300,
            "old background polling cannot restore old context"
        );
        sequencer.observe_context("s", test_context(2, 350));
        sequencer.observe_context("s", test_context(2, 300));
        assert_eq!(
            sequencer.session_total_tokens("s"),
            350,
            "same-call stale usage cannot retract completed output"
        );
        sequencer.observe_context("other", test_context(3, 100));
        assert_eq!(sequencer.session_total_tokens("s"), 350);
        assert_eq!(sequencer.session_total_tokens("other"), 100);
    }

    #[test]
    fn observed_context_is_not_clamped_to_hide_budget_overflow() {
        let sequencer = ProjectionSequencer::new();
        sequencer.observe_context("s", test_context(1, 1_500));
        assert_eq!(sequencer.session_total_tokens("s"), 1_500);
    }

    #[test]
    fn stamp_update_meta_carries_event_tokens_prompt_and_replay_keys() {
        let fresh = stamp_update_meta(
            "s-1",
            64,
            Some("prompt-9"),
            None,
            UpdateTimestamps {
                agent_timestamp_ms: Some(1_700_000_003_200),
                stream_start_ms: Some(1_700_000_000_000),
                turn_start_ms: Some(1_699_999_999_000),
            },
        );
        assert_eq!(fresh["eventId"], "s-1");
        assert_eq!(fresh["totalTokens"], 64);
        assert_eq!(fresh["promptId"], "prompt-9");
        assert_eq!(fresh["agentTimestampMs"], 1_700_000_003_200i64);
        assert_eq!(fresh["streamStartMs"], 1_700_000_000_000i64);
        assert_eq!(fresh["turnStartMs"], 1_699_999_999_000i64);
        assert!(
            fresh.get("isReplay").is_none(),
            "fresh updates omit the key"
        );

        let replay = stamp_update_meta("s-2", 64, None, Some(true), UpdateTimestamps::default());
        assert_eq!(replay["isReplay"], true);
        assert!(replay.get("promptId").is_none());

        let echo = stamp_update_meta(
            "s-3",
            0,
            Some("prompt-1"),
            Some(false),
            UpdateTimestamps::default(),
        );
        assert_eq!(echo["isReplay"], false);
        assert_eq!(echo["promptId"], "prompt-1");
        assert_eq!(echo["totalTokens"], 0);
    }

    #[test]
    fn session_update_notification_wraps_payload_with_session_and_meta() {
        let meta = stamp_update_meta(
            "session-1-1",
            64,
            Some("prompt-1"),
            None,
            UpdateTimestamps::default(),
        );
        let notification = session_update_notification(
            "session-1",
            json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "hi"},
            }),
            meta,
        );
        assert_eq!(notification["jsonrpc"], "2.0");
        assert_eq!(notification["method"], "session/update");
        assert_eq!(notification["params"]["sessionId"], "session-1");
        assert_eq!(
            notification["params"]["update"]["sessionUpdate"],
            "agent_message_chunk"
        );
        // The Grok decoder expects the chunk field name `content`.
        assert_eq!(notification["params"]["update"]["content"]["text"], "hi");
        assert_eq!(notification["params"]["_meta"]["promptId"], "prompt-1");
        assert_eq!(notification["params"]["_meta"]["eventId"], "session-1-1");
        assert_eq!(notification["params"]["_meta"]["totalTokens"], 64);
        let extension = session_notification_for_method(
            SUBAGENT_NOTIFICATION_METHOD,
            "session-1",
            json!({"sessionUpdate": "subagent_spawned"}),
            json!({}),
        );
        assert_eq!(
            extension["method"], "_x.ai/session_notification",
            "ACP SDK requires the extension marker on serialized methods"
        );
    }

    #[test]
    fn bound_context_window_falls_back_to_runtime_default() {
        let zeroed = BoundModelContext::new("b::m".to_string(), "m".to_string(), 0);
        assert_eq!(
            zeroed.effective_context_window(),
            gents::DEFAULT_CONTEXT_WINDOW as u64,
            "the Grok fallback must come from the runtime's single context-window default"
        );
        let pinned = BoundModelContext::new("b::m".to_string(), "m".to_string(), 8_192);
        assert_eq!(pinned.effective_context_window(), 8_192);
    }

    #[test]
    fn bound_model_context_keeps_model_id_and_display_name() {
        let bound = BoundModelContext::new(
            "GLM-5.3-NVFP4".to_string(),
            "GLM-5.3-NVFP4".to_string(),
            262_144,
        );
        assert_eq!(bound.model_id, "GLM-5.3-NVFP4");
        assert_eq!(bound.model_name, "GLM-5.3-NVFP4");
        assert_eq!(bound.total_context_tokens, 262_144);
    }

    async fn seed_bound_inference(node: &EmbeddedNode, window: Option<i64>) {
        use gents::config_client::{
            apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument,
            DesiredStateApplyPlan,
        };
        use gents::Collection;
        let owner = "did:grok-binding-test";
        gents::ensure_agent_principal(node, owner).await.unwrap();
        let plan = DesiredStateApplyPlan::new([
            (Collection::AgentBehavior, json!({"agent_did":owner,"behavior_id":"port-live","inference_profile_id":"profile"})),
            (Collection::InferenceProfile, json!({"agent_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"GLM-5.3-NVFP4","context_window":window})),
            (Collection::InferenceBackend, json!({"agent_did":owner,"backend_id":"backend","name":"Workstation","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1","auth":{"kind":"unauthenticated"}})),
        ].into_iter().map(|(collection,value)| DesiredStateApplyDocument {collection,add:value.clone(),update:value}).collect()).unwrap();
        ConfigAccess::transact_local(node, None, "grok.binding.fixture", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn resolve_bound_model_context_projects_the_production_style_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let node = EmbeddedNode::builder()
            .data_path(dir.path().join("node"))
            .build()
            .await
            .unwrap();
        gents::ensure_runtime_schemas(&node).await.unwrap();
        seed_bound_inference(&node, Some(262_144)).await;
        let bound = resolve_bound_model_context(&node, "did:grok-binding-test", "port-live")
            .await
            .unwrap();
        assert_eq!(bound.model_id, "GLM-5.3-NVFP4");
        assert_eq!(bound.model_name, "GLM-5.3-NVFP4");
        assert_eq!(bound.total_context_tokens, 262_144);
        assert_eq!(bound.effective_context_window(), 262_144);
        assert!(
            resolve_bound_model_context(&node, "did:foreign", "port-live")
                .await
                .is_err()
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn resolve_bound_model_context_uses_default_only_for_missing_window() {
        let dir = tempfile::tempdir().unwrap();
        let node = EmbeddedNode::builder()
            .data_path(dir.path().join("node"))
            .build()
            .await
            .unwrap();
        gents::ensure_runtime_schemas(&node).await.unwrap();
        seed_bound_inference(&node, None).await;
        let bound = resolve_bound_model_context(&node, "did:grok-binding-test", "port-live")
            .await
            .unwrap();
        assert_eq!(bound.model_id, "GLM-5.3-NVFP4");
        assert_eq!(
            bound.total_context_tokens,
            gents::DEFAULT_CONTEXT_WINDOW as u64
        );
        assert_eq!(
            bound.effective_context_window(),
            gents::DEFAULT_CONTEXT_WINDOW as u64
        );
        assert!(
            resolve_bound_model_context(&node, "did:grok-binding-test", "missing")
                .await
                .is_err()
        );
        node.shutdown().await;
    }

    /// One durable assistant row carrying both a reasoning thought and body
    /// text streams as two chunks, and a cursor that only recorded the
    /// thought (its send failed) still emits the text on the next poll —
    /// the chunk-level identity is what makes the retry recover the second
    /// chunk instead of dropping it with the row.
    #[tokio::test]
    async fn chunk_level_identity_recovers_a_partial_row_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            EmbeddedNode::builder()
                // The staging `TempDir` guard stays in scope (`dir`) for the
                // test's lifetime, so the node's storage directory is deleted
                // when the test ends — never abandoned with `keep()` or
                // leaked with `mem::forget`.
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
            request.agent_did.as_deref().unwrap(),
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

        // First poll: both chunks of the row are novel.
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

        // Simulate a partial send failure: only the thought's send
        // succeeded, so only its advance is recorded. The text chunk's
        // identity stays unseen and must be re-emitted by the next poll.
        cursor.record(first[0].advance.clone());
        let second = engine
            .project_request_updates(&request, &mut cursor, None)
            .await
            .expect("second poll");
        assert_eq!(
            second.len(),
            1,
            "only the unsent text chunk re-emits; the delivered thought does not duplicate"
        );
        assert_eq!(
            second[0].payload["sessionUpdate"], "agent_message_chunk",
            "the retry recovers the text chunk, not the thought"
        );

        // After the retry's send succeeds, a third poll emits nothing.
        cursor.record(second[0].advance.clone());
        let third = engine
            .project_request_updates(&request, &mut cursor, None)
            .await
            .expect("third poll");
        assert!(third.is_empty(), "every chunk is now delivered");
    }

    /// Seed the embedded node with runtime schemas and start a projection
    /// engine, the production shape every embedded chronology test uses.
    async fn embedded_engine() -> (tempfile::TempDir, Arc<ProjectionEngine>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            EmbeddedNode::builder()
                // The staging `TempDir` guard stays in scope (`dir`) for the
                // test's lifetime, so the node's storage directory is deleted
                // when the test ends — never abandoned with `keep()` or
                // leaked with `mem::forget`.
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
        let result = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{purpose: "normal", request_id: "{}", session_id: "{}", agent_did: "did:test:grok-shim", requester_did: "did:test:grok-shim", behavior_id: "test", content: "projection fixture", lifecycle_state: "pending"}}) {{_docID}} }}"#, gents::graphql::escape_graphql_string(request_id), gents::graphql::escape_graphql_string(session))).await;
        ensure_no_errors(&result, "seed projection request").unwrap();
        let doc = gents_protocol::graphql::extract_mutation_doc_id(
            &json!({"data":result.data}),
            "AgentRequest",
        )
        .unwrap();
        serde_json::from_value(json!({"_docID":doc,"request_id":request_id,"agent_did":"did:test:grok-shim","requester_did":"did:test:grok-shim","session_id":session})).unwrap()
    }

    /// Seed one durable `AgentToolCall` row with an explicit stable id and
    /// transcript sequence.
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
                    agent_did: "did:test:grok-shim"
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

    /// Bind already-created physical tool rows to their one coordinator
    /// admission header. Tool payloads live in canonical closed segments;
    /// AgentToolCall rows carry lifecycle and identity, not arguments.
    async fn seed_canonical_tool_admission_header(
        node: &EmbeddedNode,
        request: &gents_protocol::row::AgentRequestRow,
        sequence: u32,
        calls: &[(&str, &str, &str)],
    ) {
        use gents::defra_node::{ExecuteRetryPolicy, QueryRequest};
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

        let agent_did = request.agent_did.as_deref().unwrap();
        let session_id = request.session_id.as_deref().unwrap();
        let request_doc_id = request.doc_id.as_deref().unwrap();
        let generation = "fixture:tool-admission";
        let created_at = "2026-08-31T22:46:44Z";
        let mut blocks = Vec::new();
        for (block_index, (tool_doc_id, native_id, name)) in calls.iter().enumerate() {
            let arguments = if *name == "create_session" {
                r#"{"target":"child-chron"}"#
            } else {
                r#"{"command":"true"}"#
            };
            let segment = OutputSegment {
                agent_did: agent_did.into(),
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
                .execute_request_with_retry(
                    QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                        .with_variables(output_segment_create_variables(&segment).unwrap()),
                    ExecuteRetryPolicy::default(),
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
                agent_did,
                session_id,
                request.requester_did.as_deref(),
                sequence,
            ),
            session_id: session_id.into(),
            agent_did: agent_did.into(),
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
            .execute_request_with_retry(
                QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                    .with_variables(transcript_message_create_variables(&message).unwrap()),
                ExecuteRetryPolicy::default(),
            )
            .await;
        assert!(
            !response.has_errors(),
            "tool header seed: {:?}",
            response.errors
        );
    }

    /// Seed the first `AgentRequest` of a session caused by the parent
    /// request, with an explicit equal-time `created_at`.
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
        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(input: {{purpose: "normal", 
                    request_id: "{escaped_child}"
                    agent_did: "did:test:grok-shim"
                    requester_did: "did:test:grok-shim"
                    behavior_id: "test-child"
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

    /// The `sessionUpdate` kind of one novel event, for order assertions.
    fn update_kind(event: &NovelProjectionEvent) -> String {
        event.payload["sessionUpdate"]
            .as_str()
            .expect("sessionUpdate kind")
            .to_string()
    }

    /// The mixed durable chronology, through the production projection
    /// engine over an embedded node and runtime schemas:
    ///
    /// - one assistant `AgentMessage` row at sequence 3 (a reasoning thought
    ///   plus body text, streamed as two chunks);
    /// - two `AgentToolCall` rows at the *same* `message_sequence` 4, seeded
    ///   in reverse stable-identity order;
    /// - the first `AgentRequest` of a session caused by the `call-a`
    ///   `create_session` call, whose position is that call's sequence.
    ///
    /// The wire order must be exactly: thought, text, tool a, tool z,
    /// spawned, with the positionless `available_commands_update` last —
    /// and the same on a re-poll after a failed send, without duplicating
    /// the events whose sends succeeded.
    #[tokio::test]
    async fn persisted_context_hydrates_metadata_without_a_response_token_counter() {
        let (_dir, engine) = embedded_engine().await;
        let response = engine.node.execute(r#"mutation { create_AgentRequest(input: {purpose: "normal", 
            request_id: "context-owner", session_id: "context-session", agent_did: "did:test:grok-shim",
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
            "agent_did":"did:test:grok-shim", "requester_did":"did:test:requester"
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
            agent_did: "did:test:grok-shim", call_kind: "inference", call_seq: 1,
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

        // The assistant turn's durable message: reasoning before text,
        // published through the canonical row owner (closed authored
        // reasoning and text segments plus the RequestExecution header).
        let message_key = gents::session::sequence_message_key(
            request.agent_did.as_deref().unwrap(),
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

        // Two same-sequence tool calls seeded in REVERSE stable order: the
        // projection must emit `call-a` before `call-z` by identity. The
        // first is the `create_session` call that caused the child session.
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
            "create_session",
            4,
        )
        .await;
        seed_canonical_tool_admission_header(
            &engine.node,
            &request,
            4,
            &[
                (&spawn_tool_doc_id, "call-a", "create_session"),
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
            ],
            "the mixed payload must merge by chronology with family-rank ties and a positionless tail"
        );
        // The same-sequence tools emitted in stable-identity order, not
        // insertion order.
        let tool_ids: Vec<&str> = first
            .iter()
            .filter(|event| event.payload["sessionUpdate"] == "tool_call")
            .map(|event| event.payload["toolCallId"].as_str().expect("toolCallId"))
            .collect();
        assert_eq!(
            tool_ids,
            vec!["call-a", "call-z"],
            "same-sequence tools must emit in stable identity order"
        );
        // The pager routes subagent lifecycle updates by the caused session
        // id, never by the causing tool call id.
        let spawned = first
            .iter()
            .find(|event| event.payload["sessionUpdate"] == "subagent_spawned")
            .expect("spawned event");
        assert_eq!(spawned.payload["subagent_id"], "s-chron-child");

        // Failed later send: record only the first three advances (thought,
        // text, tool-a base). Its terminal update and all later events must
        // reappear in the same deterministic order on the next poll.
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
            ],
            "the failed events reappear in the same deterministic remaining order; the delivered ones never duplicate"
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

        // Deliver the rest; a final poll is empty.
        for advance in second.iter().map(|event| event.advance.clone()) {
            cursor.record(advance);
        }
        let third = engine
            .project_request_updates(&request, &mut cursor, None)
            .await
            .expect("third poll");
        assert!(third.is_empty(), "every event is now delivered");
    }

    #[test]
    fn canonical_live_cursor_advances_only_after_send_success() {
        let mut cursor = LiveSegmentCursor::default();
        let (first, plan) = cursor.plan("hello").expect("first canonical prefix");
        assert_eq!(first, "hello");
        assert_eq!(
            cursor.plan("hello").map(|candidate| candidate.0),
            Some("hello".to_string()),
            "an uncommitted send must replay"
        );
        cursor.commit(plan);
        assert!(cursor.plan("hello").is_none());
        let (suffix, plan) = cursor.plan("hello world").expect("grown canonical prefix");
        assert_eq!(suffix, " world");
        cursor.commit(plan);
        assert_eq!(cursor.sent_bytes, "hello world");
    }

    #[test]
    fn canonical_live_divergence_preserves_nonrewind_evidence() {
        let mut cursor = LiveSegmentCursor {
            segment_key: Some("source-a".into()),
            ..Default::default()
        };
        let (_, first) = cursor.plan("old bytes").expect("initial prefix");
        cursor.commit(first);

        let (replacement, replacement_plan) = cursor.plan("new").expect("replacement prefix");
        assert_eq!(replacement, "new");
        assert_eq!(replacement_plan.closed_evidence.len(), 1);
        assert_eq!(replacement_plan.closed_evidence[0].sent_bytes, "old bytes");
        assert_eq!(
            replacement_plan.closed_evidence[0].segment_key.as_deref(),
            Some("source-a")
        );
        assert_eq!(cursor.sent_bytes, "old bytes", "planning never rewinds");
    }

    #[test]
    fn identical_text_from_distinct_canonical_sources_is_not_collapsed() {
        let mut cursor = LiveSegmentCursor::default();
        cursor.begin_source("source-a");
        let (first, plan) = cursor.plan("same text").expect("first source");
        assert_eq!(first, "same text");
        cursor.commit(plan);

        cursor.begin_source("source-b");
        let (second, plan) = cursor.plan("same text").expect("distinct source");
        assert_eq!(second, "same text");
        assert_eq!(plan.closed_evidence.len(), 1);
        assert_eq!(
            plan.closed_evidence[0].segment_key.as_deref(),
            Some("source-a")
        );
    }

    #[test]
    fn canonical_header_binding_requires_exact_source_identity() {
        let row = DurableRowView {
            identity: DurableRowIdentity {
                sequence: 7,
                message_key: "header-7".into(),
            },
            content: "same text".into(),
            reasoning: String::new(),
        };
        let mut live = LiveSegmentCursor {
            segment_key: Some("source-a".into()),
            sent_bytes: "same text".into(),
            ..Default::default()
        };
        let binding = messages::CanonicalSourceBinding {
            source_key: "source-b".into(),
            sequence: 7,
            message_key: "header-7".into(),
        };
        bind_canonical_source_evidence(
            &mut live,
            &binding,
            &row.identity,
            EvidenceRail::Content,
            std::slice::from_ref(&row),
        );
        assert!(live.closed_evidence.is_empty());

        let matching = messages::CanonicalSourceBinding {
            source_key: "source-a".into(),
            sequence: 7,
            message_key: "header-7".into(),
        };
        bind_canonical_source_evidence(
            &mut live,
            &matching,
            &row.identity,
            EvidenceRail::Content,
            std::slice::from_ref(&row),
        );
        assert_eq!(live.closed_evidence.len(), 1);
    }
}
