//! Grok shim projection engine root.
//!
//! The projection engine owns the connection-local side of the Grok shim: it
//! turns durable Gents rows (`AgentResponse`, `AgentMessage`,
//! `AgentToolCall`/`AgentToolResult`, and runtime child `AgentRequest` rows)
//! into fresh Grok pager `session/update` notification payloads and stamps the
//! per-connection event metadata (`_meta.eventId`, `_meta.promptId`,
//! `_meta.totalTokens`) those payloads require.
//!
//! The engine is deliberately bounded and request-id-scoped:
//! - every projection helper takes an explicit request id and queries only the
//!   rows that request can own (one query per row family, no graph walks
//!   beyond the direct children of the projected request);
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
//! - [`subagents`]: subagent spawned/progress/finished updates from runtime
//!   child `AgentRequest` rows and the shaped not-found ext stubs.
//!
//! Static `Task` configuration rows are never treated as runtime state and no
//! permission or terminal documents are ever fabricated here.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use gents::{load_agent_behavior, load_inference_profile};
use serde_json::{json, Map, Value};

pub(crate) mod messages;
pub(crate) mod subagents;
pub(crate) mod tools;

/// Wire name of the ACP `session/update` notification every projection
/// payload is wrapped in.
pub(crate) const SESSION_UPDATE_METHOD: &str = "session/update";

/// Default context window reported when the bound configuration does not
/// supply one. Mirrors the model catalog's `totalContextTokens` default scale
/// (`gents::DEFAULT_CONTEXT_WINDOW`) so a bound behavior that never pinned a
/// window still reports a truthful, bounded value instead of zero.
pub(crate) const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 262_144;

/// Bound model/context configuration the shim was assembled with.
///
/// Model and context-window values come from the bound `AgentBehavior` and its
/// `InferenceProfile`, not from `AgentSession` (which has no model or
/// context-window fields).
#[derive(Debug, Clone)]
pub(crate) struct BoundModelContext {
    /// Grok `modelId` the pager addresses: the bound behavior's `model_name`
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
        if self.total_context_tokens == 0 {
            DEFAULT_CONTEXT_WINDOW_TOKENS
        } else {
            self.total_context_tokens
        }
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
/// - `totalTokens` is cumulative and never decreases within a session.
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
/// session-cumulative token total.
#[derive(Debug, Default)]
struct SessionSequence {
    event_counter: u64,
    total_tokens: u64,
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

    /// The session-cumulative token total for `session_id`.
    pub(crate) fn session_total_tokens(&self, session_id: &str) -> u64 {
        self.sessions
            .lock()
            .expect("grok shim sequencer lock poisoned")
            .get(session_id)
            .map(|sequence| sequence.total_tokens)
            .unwrap_or(0)
    }

    /// Apply one per-request token observation to the session-cumulative
    /// total.
    ///
    /// `AgentResponse.token_count` is a *per-request* observation, while
    /// `_meta.totalTokens` must be *session-cumulative and never
    /// decreasing*. The request-local `high_water` cursor reconciles the
    /// two: only the positive delta above the highest observation this
    /// request has already applied is added — exactly once — so repeated
    /// polls of the same value never double-count and stale or decreasing
    /// observations (a retry-replaced response row, a late poll) are
    /// ignored. The cumulative total is clamped to the bound context window
    /// without ever decreasing.
    ///
    /// The high-water advance is recorded at poll time, not send time: it is
    /// an *observation* record rather than a delivery record, which is
    /// exactly what makes a re-poll after a failed send add nothing the
    /// second time.
    pub(crate) fn apply_token_observation(
        &self,
        session_id: &str,
        high_water: &mut u64,
        observed: u64,
        context_window_tokens: u64,
    ) {
        if observed <= *high_water {
            // A repeated, stale, or decreasing observation: the delta was
            // already applied (or never existed).
            return;
        }
        let delta = observed - *high_water;
        *high_water = observed;
        let mut sessions = self
            .sessions
            .lock()
            .expect("grok shim sequencer lock poisoned");
        let sequence = sessions.entry(session_id.to_string()).or_default();
        let candidate = sequence.total_tokens.saturating_add(delta);
        // Clamp to the window without ever decreasing: a bound window that
        // shrank mid-session cannot retract tokens already reported.
        sequence.total_tokens = candidate
            .min(context_window_tokens)
            .max(sequence.total_tokens);
    }
}

/// Build the `_meta` object stamped on one session/update notification.
///
/// Fields follow the pager's `NotificationMeta`: `eventId` is
/// `"{sessionId}-{counter}"`, `totalTokens` is the cumulative session usage,
/// and `promptId` correlates the update with its turn. `is_replay` is
/// `None` for fresh updates (the key is omitted entirely) and `Some(false)`
/// for the user echo, which carries the key explicitly.
pub(crate) fn stamp_update_meta(
    event_id: &str,
    total_tokens: u64,
    prompt_id: Option<&str>,
    is_replay: Option<bool>,
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
    Value::Object(meta)
}

/// Wrap one projected update payload in a `session/update` notification
/// envelope.
///
/// The Grok decoder expects the chunk field name `content` (not
/// `contentBlock`); the leaves own that shape and this wrapper only adds the
/// session envelope and the stamped `_meta`.
pub(crate) fn session_update_notification(session_id: &str, update: Value, meta: Value) -> Value {
    let mut params = Map::new();
    params.insert(
        "sessionId".to_string(),
        Value::String(session_id.to_string()),
    );
    params.insert("update".to_string(), update);
    params.insert("_meta".to_string(), meta);
    json!({
        "jsonrpc": "2.0",
        "method": SESSION_UPDATE_METHOD,
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

    /// The connection's projection sequencer as a shared handle, for tests
    /// that inspect per-session counters.
    pub(crate) fn sequencer_arc(&self) -> Arc<ProjectionSequencer> {
        self.sequencer.clone()
    }

    /// Poll the durable request-scoped projections and return only the
    /// *novel* events this cursor has not emitted yet, merged across
    /// families into durable transcript chronology (see step 4 below).
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
    /// - tool calls: the first observation of a `tool_call` base emits the
    ///   full tracker registration; a later change to the tracked fields
    ///   (`title`/`kind`/`status`/`content`/`rawInput`/`rawOutput`/`meta`)
    ///   emits a `tool_call_update` carrying exactly the changed fields. The
    ///   leaves' own redundant `tool_call_update` events are ignored: the
    ///   base payload already carries the current authoritative status.
    ///   `available_commands_update` emits once per distinct visible tool
    ///   list.
    /// - subagents: one event per distinct payload per
    ///   `<sessionUpdate kind>:<subagentId>`; a still-running child's
    ///   `durationMs` is 0 (the elapsed computation needs a terminal bound),
    ///   so running progress payloads are stable across polls.
    /// - messages: one event per durable `AgentMessage` row ordinal, so
    ///   distinct rows with identical text both emit. The synthetic
    ///   `user_message_chunk` echo of the current prompt's user row is
    ///   skipped — the turn already echoed the prompt blocks directly.
    ///
    /// The message projection's per-request `token_count` observation is
    /// applied here — at poll time, not send time — through
    /// [`ProjectionSequencer::apply_token_observation`] with the caller's
    /// request-local high-water cursor, so the session-cumulative
    /// `totalTokens` advances by the positive delta exactly once even if
    /// this poll's sends fail and the next poll re-observes the same value.
    pub(crate) async fn project_request_updates(
        &self,
        session_id: &str,
        request_id: &str,
        token_high_water: &mut u64,
        cursor: &mut RequestCursor,
    ) -> Result<Vec<NovelProjectionEvent>> {
        // Each family projects independently (one bounded query set per
        // leaf), then the novel events merge into one chronology below.
        let mut merged: Vec<MergedEvent> = Vec::new();

        // 1. Tools (lifecycle of the request's tool calls).
        let tools = tools::project_tools(&self.node, request_id, session_id).await?;
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
                            payload: emitted,
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
                tools::ToolUpdate::ToolCallUpdate(_) => {
                    // Redundant with the base payload's current status: the
                    // base-derived diff above is the single source of tool
                    // lifecycle updates.
                }
                tools::ToolUpdate::AvailableCommands(update) => {
                    let payload = update.to_payload();
                    let fingerprint = payload_fingerprint(&payload);
                    let Some(advance) = cursor.commands_changed(fingerprint) else {
                        continue;
                    };
                    merged.push(MergedEvent {
                        event: NovelProjectionEvent { payload, advance },
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

        // 2. Subagents (runtime child requests).
        let subagents = subagents::project_subagents(
            self.node.as_ref(),
            request_id,
            session_id,
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
                event: NovelProjectionEvent { payload, advance },
                chronology,
                family_rank: FAMILY_RANK_SUBAGENT,
                family_ordinal: merged
                    .iter()
                    .filter(|item| item.family_rank == FAMILY_RANK_SUBAGENT)
                    .count(),
            });
        }

        // 3. Messages (assistant/user transcript chunks). The user echo of
        // the current prompt is skipped: the turn already sent it directly.
        let messages = messages::project_messages(
            &self.node,
            request_id,
            self.bound.effective_context_window(),
        )
        .await?;
        self.sequencer.apply_token_observation(
            session_id,
            token_high_water,
            messages.total_tokens,
            self.bound.effective_context_window(),
        );
        for (index, update) in messages.updates.iter().enumerate() {
            if matches!(update, messages::MessageUpdate::UserMessageChunk { .. }) {
                continue;
            }
            let Some(key) = messages.update_keys.get(index) else {
                continue;
            };
            if key.trim().is_empty() {
                continue;
            }
            if cursor.message_seen(key) {
                continue;
            }
            let chronology = messages.chronology.get(index).copied().flatten();
            merged.push(MergedEvent {
                event: NovelProjectionEvent {
                    payload: update.to_payload(),
                    advance: CursorAdvance::MessageChunk {
                        message_key: key.clone(),
                    },
                },
                chronology,
                family_rank: FAMILY_RANK_MESSAGE,
                family_ordinal: merged
                    .iter()
                    .filter(|item| item.family_rank == FAMILY_RANK_MESSAGE)
                    .count(),
            });
        }

        // 4. Cross-family merge: emit in durable chronology order, never
        // family-batched. The primary key is the durable transcript position
        // each family shares (tool `message_sequence`, message `sequence`,
        // and the subagent's spawn-tool `message_sequence` all allocate from
        // the same session transcript sequence space), so a client replaying
        // the stream observes tool calls, subagent lifecycles, and message
        // chunks in the order the transcript recorded them. Ties break by
        // family rank: message chunks of an assistant turn precede the tool
        // call that turn issued (thought-before-text precedes the call), and
        // a `subagent_spawned` follows its spawn tool call. Within a family,
        // equal positions break by the durable stable identity each family's
        // decoded rows were sorted by (the tool call's stable id, the spawn
        // row's tool call id, the child's request id), so the merged wire
        // order is a pure function of the durable rows and never of query
        // iteration order. Positionless events
        // (`available_commands_update`, rows without a sequence, and
        // subagents without a spawn row) sort after every positioned event
        // of their family, preserving each family's own emission order.
        merged.sort_by(|a, b| family_sort_key(a).cmp(&family_sort_key(b)));
        Ok(merged.into_iter().map(|item| item.event).collect())
    }
}

/// Family ranks for the cross-family merge at equal chronology. Lower rank
/// emits first: message chunks (reasoning precedes the assistant turn's tool
/// call), then the tool call, then the subagent that spawn tool created.
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
    /// The `session/update` payload (`sessionUpdate` object) to wrap and
    /// send.
    pub(crate) payload: Value,
    /// The advance that records this event as delivered once it is sent.
    pub(crate) advance: CursorAdvance,
}

/// The recorded identity of one novel projection event. Recorded only after
/// the corresponding notification line was successfully sent.
#[derive(Debug, Clone)]
pub(crate) enum CursorAdvance {
    /// The full base payload of a tool call was observed (first time or
    /// changed tracked fields).
    ToolBase {
        tool_call_id: String,
        payload: Value,
    },
    /// A distinct visible tool list was observed.
    Commands { fingerprint: u64 },
    /// A distinct subagent payload was observed for its key.
    Subagent { key: String, fingerprint: u64 },
    /// A durable message row was observed by its key.
    MessageChunk { message_key: String },
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
    "meta",
];

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
/// - messages: the set of durable `AgentMessage` keys already streamed.
///
/// The poll computes novel events against the cursor without mutating it;
/// the caller records each advance after the send succeeds, so a send
/// failure replays the same events on the next poll instead of dropping
/// them.
#[derive(Debug, Default)]
pub(crate) struct RequestCursor {
    /// Last-sent base payload per tool call id.
    tool_bases: BTreeMap<String, Value>,
    /// Last-sent visible tool list fingerprint.
    commands_state: Option<u64>,
    /// Last-sent payload fingerprint per subagent key.
    subagent_states: BTreeMap<String, u64>,
    /// Durable message keys already streamed.
    message_chunks: BTreeMap<String, ()>,
}

impl RequestCursor {
    pub(crate) fn new() -> Self {
        Self::default()
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
                let fields = changed_tool_fields(last_sent, payload)?;
                Some((
                    json!({
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": tool_call_id,
                        "fields": fields,
                    }),
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

    /// Whether the subagent payload is novel for its key.
    fn subagent_changed(&mut self, key: &str, fingerprint: u64) -> Option<CursorAdvance> {
        if self.subagent_states.get(key) == Some(&fingerprint) {
            return None;
        }
        Some(CursorAdvance::Subagent {
            key: key.to_string(),
            fingerprint,
        })
    }

    /// Whether the durable message key has not been streamed yet.
    fn message_seen(&mut self, message_key: &str) -> bool {
        self.message_chunks.contains_key(message_key)
    }

    /// Record one delivered event after its send succeeded.
    pub(crate) fn record(&mut self, advance: CursorAdvance) {
        match advance {
            CursorAdvance::ToolBase {
                tool_call_id,
                payload,
            } => {
                self.tool_bases.insert(tool_call_id, payload);
            }
            CursorAdvance::Commands { fingerprint } => {
                self.commands_state = Some(fingerprint);
            }
            CursorAdvance::Subagent { key, fingerprint } => {
                self.subagent_states.insert(key, fingerprint);
            }
            CursorAdvance::MessageChunk { message_key } => {
                self.message_chunks.insert(message_key, ());
            }
        }
    }
}

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

/// Resolve the bound model/context configuration for the shim from the bound
/// behavior's `AgentBehavior` and `InferenceProfile` documents.
///
/// `AgentBehavior` selects `model_name` and `backend_id`; `InferenceProfile`
/// owns the context window. `AgentSession` has no model or context-window
/// fields and is never consulted here. Failures are surfaced as errors instead
/// of being papered over with a synthetic catalog entry.
pub(crate) async fn resolve_bound_model_context(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<BoundModelContext> {
    let behavior = load_agent_behavior(node, behavior_id)
        .await
        .with_context(|| format!("loading AgentBehavior {behavior_id:?} for the Grok shim"))?
        .ok_or_else(|| {
            anyhow!(
                "Grok shim is bound to behavior {behavior_id:?}, but no AgentBehavior document \
                 with that behavior_id exists"
            )
        })?;
    let model_name = behavior
        .model_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            anyhow!(
                "Grok shim is bound to behavior {behavior_id:?}, but that behavior has no \
                 model_name set, so no Grok modelId can be projected"
            )
        })?;
    // The backend selection is still validated (a bound behavior without a
    // backend cannot serve) but stays internal: it is a Gents routing
    // detail and never leaks into the wire-facing model identity.
    behavior
        .backend_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "Grok shim is bound to behavior {behavior_id:?}, but that behavior has no \
                 backend_id set, so no Grok modelId can be projected"
            )
        })?;
    let context_window = match behavior.inference_profile_id.as_deref().map(str::trim) {
        Some(profile_id) if !profile_id.is_empty() => {
            let profile = load_inference_profile(node, profile_id)
                .await
                .with_context(|| {
                    format!("loading InferenceProfile {profile_id:?} for the Grok shim")
                })?
                .ok_or_else(|| {
                    anyhow!(
                        "Grok shim is bound to behavior {behavior_id:?}, which references \
                         inference_profile_id {profile_id:?}, but no InferenceProfile document \
                         with that id exists"
                    )
                })?;
            profile
                .context_window
                .and_then(|value| u64::try_from(value.max(0)).ok())
                .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS)
        }
        _ => DEFAULT_CONTEXT_WINDOW_TOKENS,
    };

    // The pager addresses models by their `modelId` — the bound behavior's
    // `model_name` exactly. The `backend_id` stays internal: it is a Gents
    // routing detail and never leaks into the wire-facing model identity.
    Ok(BoundModelContext::new(
        model_name.clone(),
        model_name,
        context_window,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

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

    /// Gate 2 (token accounting): the token observation is applied at poll
    /// time — an *observation* record, not a delivery record — so a failed
    /// send rolls the event id back but leaves the session-cumulative
    /// `totalTokens` advanced by exactly the observed delta. The recovery
    /// send then re-observes the same value (adds nothing) and stamps the
    /// already-recorded cumulative total with the rolled-back id. This is
    /// the deliberate coherence: the failed turn's tokens were *consumed*
    /// regardless of delivery, and a live-send failure is connection-terminal
    /// for the pager anyway — the recovery path here models exactly what a
    /// re-poll through the same channel produces, with no double-count.
    #[tokio::test]
    async fn a_failed_send_rolls_back_the_id_but_never_double_counts_tokens() {
        let sequencer = Arc::new(ProjectionSequencer::new());
        let channel = SessionUpdateChannel::new(sequencer.clone());
        let sender = RecordingSender::new();

        // The projection pass observed 100 tokens for the request and
        // applied the delta to the session total at poll time.
        let mut high_water = 0u64;
        sequencer.apply_token_observation("s", &mut high_water, 100, 1_000);
        assert_eq!(sequencer.session_total_tokens("s"), 100);

        // The send fails: the id rolls back, nothing is enqueued.
        sender.fail_all.store(true, Ordering::SeqCst);
        channel
            .send("s", plain_update, sender.clone())
            .await
            .expect_err("the closed sender must fail the send");
        assert_eq!(sequencer.event_counter("s"), 0);

        // The next poll re-observes the same value: the high-water cursor
        // adds nothing — exactly the documented re-poll idempotence.
        sequencer.apply_token_observation("s", &mut high_water, 100, 1_000);
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

    #[test]
    fn token_observations_apply_positive_deltas_exactly_once() {
        let sequencer = ProjectionSequencer::new();
        let mut high_water = 0u64;
        sequencer.apply_token_observation("s", &mut high_water, 100, 1_000);
        assert_eq!(sequencer.session_total_tokens("s"), 100);
        // A repeated poll of the same observation adds nothing.
        sequencer.apply_token_observation("s", &mut high_water, 100, 1_000);
        assert_eq!(sequencer.session_total_tokens("s"), 100);
        // A later, larger observation of the same request adds only its
        // delta.
        sequencer.apply_token_observation("s", &mut high_water, 250, 1_000);
        assert_eq!(sequencer.session_total_tokens("s"), 250);
        // A stale (retry-replaced) smaller observation of the same request
        // is below the high-water and adds nothing, so the session total
        // never decreases.
        sequencer.apply_token_observation("s", &mut high_water, 40, 1_000);
        assert_eq!(sequencer.session_total_tokens("s"), 250);
        // A second request starts its own high-water at zero and its
        // observation accumulates on top of the session total — sequential
        // requests each contribute their own tokens.
        let mut second_request_high_water = 0u64;
        sequencer.apply_token_observation("s", &mut second_request_high_water, 70, 1_000);
        assert_eq!(sequencer.session_total_tokens("s"), 320);
        // Two sessions never share a token total.
        let mut other_session_high_water = 0u64;
        sequencer.apply_token_observation("other", &mut other_session_high_water, 70, 1_000);
        assert_eq!(sequencer.session_total_tokens("other"), 70);
        assert_eq!(sequencer.session_total_tokens("s"), 320);
    }

    #[test]
    fn token_totals_clamp_to_the_context_window_without_decreasing() {
        let sequencer = ProjectionSequencer::new();
        let mut high_water = 0u64;
        sequencer.apply_token_observation("s", &mut high_water, 900, 1_000);
        assert_eq!(sequencer.session_total_tokens("s"), 900);
        sequencer.apply_token_observation("s", &mut high_water, 1_500, 1_000);
        assert_eq!(sequencer.session_total_tokens("s"), 1_000);
        // A window that shrank mid-session cannot retract reported tokens.
        sequencer.apply_token_observation("s", &mut high_water, 1_500, 500);
        assert_eq!(sequencer.session_total_tokens("s"), 1_000);
    }

    #[test]
    fn stamp_update_meta_carries_event_tokens_prompt_and_replay_keys() {
        let fresh = stamp_update_meta("s-1", 64, Some("prompt-9"), None);
        assert_eq!(fresh["eventId"], "s-1");
        assert_eq!(fresh["totalTokens"], 64);
        assert_eq!(fresh["promptId"], "prompt-9");
        assert!(
            fresh.get("isReplay").is_none(),
            "fresh updates omit the key"
        );

        let replay = stamp_update_meta("s-2", 64, None, Some(true));
        assert_eq!(replay["isReplay"], true);
        assert!(replay.get("promptId").is_none());

        let echo = stamp_update_meta("s-3", 0, Some("prompt-1"), Some(false));
        assert_eq!(echo["isReplay"], false);
        assert_eq!(echo["promptId"], "prompt-1");
        assert_eq!(echo["totalTokens"], 0);
    }

    #[test]
    fn session_update_notification_wraps_payload_with_session_and_meta() {
        let meta = stamp_update_meta("session-1-1", 64, Some("prompt-1"), None);
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
    }

    #[test]
    fn bound_context_window_falls_back_to_catalog_default() {
        let zeroed = BoundModelContext::new("b::m".to_string(), "m".to_string(), 0);
        assert_eq!(
            zeroed.effective_context_window(),
            DEFAULT_CONTEXT_WINDOW_TOKENS
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

    #[tokio::test]
    async fn resolve_bound_model_context_projects_the_production_style_catalog() {
        // The production-style bound context: a workstation backend whose
        // behavior selects `GLM-5.3-NVFP4`, pinned to the pack profile with
        // a 262144-token context window. The pager addresses the model by
        // its `model_name` exactly — the backend id never leaks into the
        // wire-facing `modelId`.
        let dir = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                // The staging `TempDir` guard stays in scope (`dir`) for the
                // test's lifetime, so the node's storage directory is
                // deleted when the test ends — never abandoned with
                // `keep()` or leaked with `mem::forget`.
                .data_path(dir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Lark)
                .build()
                .await
                .expect("embedded node"),
        );
        gents::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .expect("runtime schemas");

        let seed = r#"mutation {
            create_InferenceBackend(input: {
                backend_id: "grok-port-backend-ws1",
                name: "workstation-1",
                endpoint: "http://127.0.0.1:8000/v1",
                max_concurrent: 16,
                max_queue_depth: 64,
                enabled: true
            }) { _docID }
            create_InferenceProfile(input: {
                profile_id: "grok-port-profile",
                display_name: "Grok TUI port profile",
                context_window: 262144
            }) { _docID }
            create_AgentBehavior(input: {
                behavior_id: "port-live",
                agent_did: "did:key:zGrokTuiPortAgentPlaceholder00000000000000000000000",
                display_name: "Live GLM probes through the Grok wire",
                backend_id: "grok-port-backend-ws1",
                model_name: "GLM-5.3-NVFP4",
                inference_profile_id: "grok-port-profile",
                enabled: true
            }) { _docID }
        }"#
        .to_string();
        let response = node.execute(&seed).await;
        assert!(!response.has_errors(), "seed failed: {:?}", response.errors);

        let bound = resolve_bound_model_context(node.as_ref(), "port-live")
            .await
            .expect("bound model context");
        assert_eq!(bound.model_id, "GLM-5.3-NVFP4");
        assert_eq!(bound.model_name, "GLM-5.3-NVFP4");
        assert_eq!(bound.total_context_tokens, 262_144);
        assert_eq!(bound.effective_context_window(), 262_144);
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
                .with_storage_backend(gents::defra_node::StorageBackend::Lark)
                .build()
                .await
                .expect("embedded node"),
        );
        gents::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .expect("runtime schemas");

        let request_id = "req-chunk-retry";
        let message = serde_json::to_string(&gents_protocol::message::Message::Assistant {
            id: None,
            content: vec![
                gents_protocol::message::AssistantContent::Reasoning(
                    gents_protocol::message::Reasoning::new("thinking"),
                ),
                gents_protocol::message::AssistantContent::text("answer"),
            ],
        })
        .expect("serialize assistant message");
        let escaped_message = gents::graphql::escape_graphql_string(&message);
        let escaped_request = gents::graphql::escape_graphql_string(request_id);
        let seed = format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "{escaped_request}:1"
                    session_id: "s-chunk"
                    agent_did: "did:test:grok-shim"
                    requester_did: "did:test:grok-shim"
                    request_id: "{escaped_request}"
                    sequence: 1
                    role: "assistant"
                    content: "{escaped_message}"
                }}) {{ _docID }}
            }}"#
        );
        let response = node.execute(&seed).await;
        assert!(!response.has_errors(), "seed failed: {:?}", response.errors);

        let engine = ProjectionEngine::new(
            node,
            BoundModelContext::new(
                "GLM-5.3-NVFP4".to_string(),
                "GLM-5.3-NVFP4".to_string(),
                262_144,
            ),
        );
        let mut token_high_water = 0u64;
        let mut cursor = RequestCursor::new();

        // First poll: both chunks of the row are novel.
        let first = engine
            .project_request_updates("s-chunk", request_id, &mut token_high_water, &mut cursor)
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
            .project_request_updates("s-chunk", request_id, &mut token_high_water, &mut cursor)
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
            .project_request_updates("s-chunk", request_id, &mut token_high_water, &mut cursor)
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
                .with_storage_backend(gents::defra_node::StorageBackend::Lark)
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

    /// Seed one durable `AgentToolCall` row with an explicit stable id and
    /// transcript sequence.
    async fn seed_tool_call_row(
        engine: &ProjectionEngine,
        session_id: &str,
        request_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        message_sequence: i64,
        child_request_id: Option<&str>,
    ) {
        let escaped_session = gents::graphql::escape_graphql_string(session_id);
        let escaped_request = gents::graphql::escape_graphql_string(request_id);
        let escaped_id = gents::graphql::escape_graphql_string(tool_call_id);
        let escaped_name = gents::graphql::escape_graphql_string(tool_name);
        let child_field = child_request_id
            .map(|id| {
                format!(
                    r#"child_request_id: "{}""#,
                    gents::graphql::escape_graphql_string(id)
                )
            })
            .unwrap_or_else(|| r#"child_request_id: """#.to_string());
        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{escaped_session}:{escaped_id}"
                    request_id: "{escaped_request}"
                    session_id: "{escaped_session}"
                    agent_did: "did:test:grok-shim"
                    requester_did: "did:test:grok-shim"
                    tool_call_id: "{escaped_id}"
                    tool_name: "{escaped_name}"
                    lifecycle_state: "completed"
                    result: "done"
                    message_sequence: {message_sequence}
                    {child_field}
                }}) {{ _docID }}
            }}"#
        );
        let response = engine.node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "seed tool call failed: {:?}",
            response.errors
        );
    }

    /// Seed one runtime child `AgentRequest` row linked to the parent
    /// request, with an explicit equal-time `created_at`.
    async fn seed_child_request_row(
        engine: &ProjectionEngine,
        parent_request_id: &str,
        child_request_id: &str,
        created_at: &str,
    ) {
        let escaped_parent = gents::graphql::escape_graphql_string(parent_request_id);
        let escaped_child = gents::graphql::escape_graphql_string(child_request_id);
        let escaped_created = gents::graphql::escape_graphql_string(created_at);
        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{escaped_child}"
                    agent_did: "did:test:grok-shim"
                    session_id: "s-chron-child"
                    caused_by_parent_request_id: "{escaped_parent}"
                    content: "child work"
                    status: "pending"
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
    /// - a child `AgentRequest` created by the spawn tool `call-a` (the
    ///   `call-a` row is a spawn row through its `child_request_id`), with an
    ///   `created_at` equal to nothing else deciding order — its position is
    ///   the spawn tool's sequence.
    ///
    /// The wire order must be exactly: thought, text, tool a, tool z,
    /// spawned, with the positionless `available_commands_update` last —
    /// and the same on a re-poll after a failed send, without duplicating
    /// the events whose sends succeeded.
    #[tokio::test]
    async fn mixed_families_project_in_deterministic_chronology_through_the_embedded_node() {
        let (_dir, engine) = embedded_engine().await;
        let session_id = "s-chron";
        let request_id = "req-chron";

        // The assistant turn's durable message: reasoning before text.
        let message = serde_json::to_string(&gents_protocol::message::Message::Assistant {
            id: None,
            content: vec![
                gents_protocol::message::AssistantContent::Reasoning(
                    gents_protocol::message::Reasoning::new("thinking"),
                ),
                gents_protocol::message::AssistantContent::text("answer"),
            ],
        })
        .expect("serialize assistant message");
        let escaped_message = gents::graphql::escape_graphql_string(&message);
        let escaped_request = gents::graphql::escape_graphql_string(request_id);
        let seed_message = format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "{escaped_request}:3"
                    session_id: "{session_id}"
                    agent_did: "did:test:grok-shim"
                    requester_did: "did:test:grok-shim"
                    request_id: "{escaped_request}"
                    sequence: 3
                    role: "assistant"
                    content: "{escaped_message}"
                }}) {{ _docID }}
            }}"#
        );
        let response = engine.node.execute(&seed_message).await;
        assert!(
            !response.has_errors(),
            "seed message failed: {:?}",
            response.errors
        );

        // Two same-sequence tool calls seeded in REVERSE stable order: the
        // projection must emit `call-a` before `call-z` by identity. The
        // first is the spawn tool (a recognized spawn verb via its recorded
        // `child_request_id`, not the family-suppressed `task` name), so it
        // keeps its rendered `tool_call` block and links the child.
        seed_tool_call_row(&engine, session_id, request_id, "call-z", "bash", 4, None).await;
        seed_tool_call_row(
            &engine,
            session_id,
            request_id,
            "call-a",
            "spawn_subagent",
            4,
            Some("child-chron"),
        )
        .await;
        // Equal-time children of the parent: the linked child plus an
        // unlinked-by-tool child that shares its timestamp, both tied so
        // only the durable sorts can decide order. Only the spawn-linked
        // child projects (the query filter keeps the family scoped).
        seed_child_request_row(&engine, request_id, "child-chron", "2026-08-31T22:46:45Z").await;

        let mut token_high_water = 0u64;
        let mut cursor = RequestCursor::new();
        let first = engine
            .project_request_updates(session_id, request_id, &mut token_high_water, &mut cursor)
            .await
            .expect("first poll");
        let kinds: Vec<String> = first.iter().map(update_kind).collect();
        assert_eq!(
            kinds,
            vec![
                "agent_thought_chunk".to_string(),
                "agent_message_chunk".to_string(),
                "tool_call".to_string(),
                "tool_call".to_string(),
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
        // The pager routes subagent lifecycle updates by the child session
        // id (the id the ext controls address), never by the spawn tool
        // call id; the payload key is the enum's snake_case field.
        let spawned = first
            .iter()
            .find(|event| event.payload["sessionUpdate"] == "subagent_spawned")
            .expect("spawned event");
        assert_eq!(spawned.payload["subagent_id"], "s-chron-child");

        // Failed later send: record only the first three advances (thought,
        // text, tool a). The remaining events — tool z, spawned, progress,
        // commands — must reappear in the same deterministic order on the
        // next poll, with the delivered events never duplicated.
        for advance in first.iter().take(3).map(|event| event.advance.clone()) {
            cursor.record(advance);
        }
        let second = engine
            .project_request_updates(session_id, request_id, &mut token_high_water, &mut cursor)
            .await
            .expect("second poll");
        let retry_kinds: Vec<String> = second.iter().map(update_kind).collect();
        assert_eq!(
            retry_kinds,
            vec![
                "tool_call".to_string(),
                "subagent_spawned".to_string(),
                "subagent_progress".to_string(),
                "available_commands_update".to_string(),
            ],
            "the failed events reappear in the same deterministic remaining order; the delivered ones never duplicate"
        );
        let retry_tool_id = second[0].payload["toolCallId"]
            .as_str()
            .expect("toolCallId");
        assert_eq!(retry_tool_id, "call-z");

        // Deliver the rest; a final poll is empty.
        for advance in second.iter().map(|event| event.advance.clone()) {
            cursor.record(advance);
        }
        let third = engine
            .project_request_updates(session_id, request_id, &mut token_high_water, &mut cursor)
            .await
            .expect("third poll");
        assert!(third.is_empty(), "every event is now delivered");
    }
}
