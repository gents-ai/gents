use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use defra_node::EmbeddedNode;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use serde_json::{json, Value};
use tokio::sync::{oneshot, Mutex};

use super::projection::{
    AsyncCommit, AsyncSendLine, CursorAdvance, NovelProjectionEvent, ProjectionEngine,
    RequestCursor, SessionUpdateChannel, UpdateTimestamps, SESSION_UPDATE_METHOD,
};
use super::server::AcpOutbound;

const MAX_TRANSIENT_GRAPHQL_READ_RETRIES: usize = 4;

fn transient_graphql_read_retry_delay(retry: usize) -> Duration {
    Duration::from_millis(50 * retry.max(1) as u64)
}

/// Collections whose changes can produce novel projected session events.
const PROJECTED_COLLECTIONS: [&str; 11] = [
    "AgentRequest",
    "AgentSession",
    "AgentToolCall",
    "AgentMessage",
    "AgentOutputSegment",
    "InferenceCall",
    "CompactionEntry",
    "ProviderContextReduction",
    "Goal",
    "GraphRun",
    "Trigger",
];

/// Graph-run and goal panels read through owners whose rows lie outside
/// [`PROJECTED_COLLECTIONS`]; this bounds how stale they can become.
const PROJECTION_FALLBACK: Duration = Duration::from_secs(2);

fn subscribe_projected_changes(node: &EmbeddedNode) -> Option<events::DocumentChangeSubscription> {
    gents::subscribe_collections(node, PROJECTED_COLLECTIONS)
        .inspect_err(|error| {
            tracing::warn!(%error, "Grok shim projection polls without a change feed");
        })
        .ok()
}

/// Wait for a projected change, at most [`PROJECTION_FALLBACK`]. A closed
/// feed degrades to fallback polling.
async fn wait_for_projected_change(changes: &mut Option<events::DocumentChangeSubscription>) {
    match changes.as_mut() {
        Some(subscription) => {
            if let Ok(None) = tokio::time::timeout(PROJECTION_FALLBACK, subscription.recv()).await {
                *changes = None;
            }
        }
        None => tokio::time::sleep(PROJECTION_FALLBACK).await,
    }
}

pub(super) const SCREEN_MODES: [&str; 3] = ["fullscreen", "inline", "minimal"];

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PromptBlock {
    pub kind: String,
    pub text: String,
    pub meta: Option<Value>,
}

impl PromptBlock {
    #[cfg(test)]
    pub(super) fn bash_command(&self) -> Option<String> {
        let meta = self.meta.as_ref()?;
        let command = meta
            .get("bash")
            .and_then(|bash| bash.get("command"))
            .and_then(Value::as_str)?;
        (!command.is_empty()).then(|| command.to_string())
    }

    #[cfg(test)]
    pub(super) fn has_skill_token_ranges(&self) -> bool {
        self.meta
            .as_ref()
            .and_then(|meta| meta.get("skillTokenRanges"))
            .and_then(Value::as_array)
            .is_some_and(|ranges| !ranges.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PromptRequest {
    pub session_id: String,
    pub prompt: Vec<PromptBlock>,
    pub prompt_id: Option<String>,
    pub screen_mode: Option<String>,
    pub send_now: bool,
    pub id: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct CancelNotification {
    pub session_id: String,
    pub cancel_trigger: Option<String>,
    pub rewind_if_no_output: bool,
    pub rewind_if_pristine: bool,
    pub prompt_id: Option<String>,
}

impl CancelNotification {
    #[cfg(test)]
    pub(super) fn meta(&self) -> Value {
        let mut meta = json!({
            "rewindIfNoOutput": self.rewind_if_no_output,
            "rewindIfPristine": self.rewind_if_pristine,
        });
        if let Some(trigger) = self.cancel_trigger.as_deref() {
            meta["cancelTrigger"] = json!(trigger);
        }
        if let Some(prompt_id) = self.prompt_id.as_deref() {
            meta["promptId"] = json!(prompt_id);
        }
        meta
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StopReason {
    EndTurn,
    Cancelled,
    Refusal,
    Error,
}

impl StopReason {
    pub(super) fn wire_name(self) -> &'static str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::Cancelled => "cancelled",
            StopReason::Refusal => "refusal",
            StopReason::Error => "error",
        }
    }
}

#[derive(Debug)]
struct TerminalOutcome {
    stop_reason: StopReason,
    error_message: Option<String>,
}

impl From<StopReason> for TerminalOutcome {
    fn from(stop_reason: StopReason) -> Self {
        Self {
            stop_reason,
            error_message: None,
        }
    }
}

fn prompt_result(outcome: TerminalOutcome) -> Result<Value> {
    if outcome.stop_reason == StopReason::Error {
        let message = outcome
            .error_message
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| "gents request failed".to_string());
        anyhow::bail!("{message}");
    }
    Ok(json!({"stopReason": outcome.stop_reason.wire_name()}))
}

#[derive(Clone)]
pub(super) enum PromptSender {
    Live {
        outbound: AcpOutbound,
    },
    Replay {
        inner: Arc<PromptSender>,
    },
    #[cfg(test)]
    Buffer {
        buffer: Arc<Mutex<Vec<String>>>,
    },
}

impl PromptSender {
    pub(super) async fn send_line(&self, line: String) -> Result<()> {
        match self {
            PromptSender::Live { outbound } => outbound.send(line).await,
            PromptSender::Replay { inner } => {
                let mut value: Value = serde_json::from_str(&line)?;
                value["params"]["_meta"]["isReplay"] = Value::Bool(true);
                Box::pin(inner.send_line(serde_json::to_string(&value)?)).await
            }
            #[cfg(test)]
            PromptSender::Buffer { buffer } => {
                buffer.lock().await.push(line);
                Ok(())
            }
        }
    }

    pub(super) async fn take_lines(&self) -> Vec<String> {
        match self {
            PromptSender::Live { .. } => Vec::new(),
            PromptSender::Replay { inner } => Box::pin(inner.take_lines()).await,
            #[cfg(test)]
            PromptSender::Buffer { buffer } => {
                let mut buffer = buffer.lock().await;
                std::mem::take(&mut *buffer)
            }
        }
    }

    pub(super) async fn send_user_message_chunk(
        &self,
        session_updates: &SessionUpdateChannel,
        session_id: &str,
        prompt_id: &str,
        block: &PromptBlock,
        prompt_index: usize,
    ) -> Result<()> {
        let session_id = session_id.to_string();
        let prompt_id = prompt_id.to_string();
        let block = block.clone();
        let session_for_lock = session_id.clone();
        session_updates
            .send(
                &session_for_lock,
                move |event_id, total_tokens| {
                    let meta = json!({
                        "promptId": prompt_id,
                        "isReplay": false,
                        "eventId": event_id,
                        "totalTokens": total_tokens,
                        "agentTimestampMs": chrono::Utc::now().timestamp_millis(),
                    });
                    let params = json!({
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "user_message_chunk",
                            "content": {
                                "type": "text",
                                "text": block.text,
                            },
                            "_meta": {
                                "promptIndex": prompt_index,
                                "hideFromScrollback": false,
                            },
                        },
                        "_meta": meta,
                    });
                    Ok(json!({
                        "jsonrpc": "2.0",
                        "method": SESSION_UPDATE_METHOD,
                        "params": params,
                    }))
                },
                PromptSenderLine(self),
            )
            .await
            .map(|_| ())
    }
}

pub(super) struct PromptSenderLine<'a>(pub(super) &'a PromptSender);

impl AsyncSendLine for PromptSenderLine<'_> {
    async fn send_line(&self, line: String) -> Result<()> {
        self.0.send_line(line).await
    }
}

struct ProjectionCursorCommit<'a> {
    cursor: &'a tokio::sync::Mutex<RequestCursor>,
    advance: CursorAdvance,
}

struct RequestUpdateTiming {
    turn_start_ms: i64,
    active_segment: Option<String>,
    starts: BTreeMap<String, i64>,
    last_start_ms: Option<i64>,
}

impl RequestUpdateTiming {
    fn new(turn_start_ms: i64) -> Self {
        Self {
            turn_start_ms,
            active_segment: None,
            starts: BTreeMap::new(),
            last_start_ms: None,
        }
    }

    fn resolve(
        &mut self,
        timing: Option<&super::projection::ProjectionEventTiming>,
        now_ms: i64,
    ) -> UpdateTimestamps {
        if let Some(timing) = timing {
            let start = if let Some(start) = self.starts.get(&timing.segment_key) {
                *start
            } else {
                let candidate = timing.stream_start_candidate_ms.unwrap_or(now_ms);
                let start = self.last_start_ms.map_or(candidate, |previous| {
                    candidate.max(previous.saturating_add(1))
                });
                self.starts.insert(timing.segment_key.clone(), start);
                self.last_start_ms = Some(start);
                start
            };
            self.active_segment = Some(timing.segment_key.clone());
            let agent_timestamp_ms = timing
                .agent_timestamp_candidate_ms
                .unwrap_or(now_ms)
                .max(start);
            return UpdateTimestamps {
                agent_timestamp_ms: Some(agent_timestamp_ms),
                stream_start_ms: Some(start),
                turn_start_ms: Some(self.turn_start_ms),
            };
        }
        UpdateTimestamps {
            agent_timestamp_ms: Some(now_ms),
            stream_start_ms: self
                .active_segment
                .as_ref()
                .and_then(|segment| self.starts.get(segment).copied()),
            turn_start_ms: Some(self.turn_start_ms),
        }
    }
}

impl AsyncCommit for ProjectionCursorCommit<'_> {
    async fn commit(&self) {
        self.cursor.lock().await.record(self.advance.clone());
    }
}

#[derive(Clone, Debug)]
pub(super) struct TurnManagerConfig {
    pub actor: identity::Did,
    pub node_did: String,
    pub agent_id: String,
    pub graphql: String,
}

#[derive(Debug, Default)]
struct CancelBeforeIdLatch {
    cancelled: bool,
}

impl CancelBeforeIdLatch {
    fn cancel(&mut self) -> bool {
        let was_cancelled = self.cancelled;
        self.cancelled = true;
        !was_cancelled
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

struct PendingPrompt {
    response_tx: Option<oneshot::Sender<Result<Value>>>,
    request: Option<gents_protocol::row::AgentRequestRow>,
    cancel_before_id: CancelLatch,
    drained: bool,
}

impl PendingPrompt {
    fn resolve(&mut self, result: Result<Value>) {
        if let Some(tx) = self.response_tx.take() {
            let _ = tx.send(result);
        }
        self.drained = true;
    }
}

/// A pending prompt's session id and prompt id.
type PromptKey = (String, String);
type CancelLatch = Arc<Mutex<CancelBeforeIdLatch>>;

#[derive(Default)]
struct ConnectionState {
    closed: bool,
    entries: RapidHashMap<PromptKey, PendingPrompt>,
}

struct ObservedRequest {
    prompt_id: Option<String>,
    cursor: Mutex<RequestCursor>,
    timing: RequestUpdateTiming,
    completion_sent: bool,
    echo_sent: bool,
}

impl ObservedRequest {
    fn new(prompt_id: String, started_at: i64, foreground: bool) -> Self {
        Self {
            prompt_id: (!foreground).then_some(prompt_id),
            cursor: Mutex::new(RequestCursor::new()),
            timing: RequestUpdateTiming::new(started_at),
            completion_sent: foreground,
            echo_sent: true,
        }
    }
}

type ObservedRequests = BTreeMap<(String, String), Arc<Mutex<ObservedRequest>>>;

type ForegroundDeliveries = Arc<std::sync::Mutex<RapidHashMap<String, usize>>>;

pub(super) struct ForegroundDelivery {
    sessions: ForegroundDeliveries,
    session_id: String,
}

impl Drop for ForegroundDelivery {
    fn drop(&mut self) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = sessions.get_mut(&self.session_id) {
            *count -= 1;
            if *count == 0 {
                sessions.remove(&self.session_id);
            }
        }
    }
}

impl ConnectionState {
    fn is_closed(&self) -> bool {
        self.closed
    }
}

pub(super) struct TurnManager {
    node: Arc<EmbeddedNode>,
    config: TurnManagerConfig,
    state: Mutex<ConnectionState>,
    delivery_gate: Mutex<()>,
    foreground_deliveries: ForegroundDeliveries,
    autonomous_delivery: Mutex<RapidHashMap<String, String>>,
    observed: Mutex<ObservedRequests>,
    observers: Mutex<RapidHashMap<String, tokio::task::JoinHandle<()>>>,
    #[cfg(test)]
    insertion_gate: Mutex<Option<TestGate>>,
    #[cfg(test)]
    disconnect_gate: Mutex<Option<TestGate>>,
    #[cfg(test)]
    cancel_drain_gate: Mutex<Option<TestGate>>,
    #[cfg(test)]
    cancel_selection_gate: Mutex<Option<TestGate>>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct TestGate {
    arrived: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl TurnManager {
    pub(super) fn new(node: Arc<EmbeddedNode>, config: TurnManagerConfig) -> Self {
        TurnManager {
            node,
            config,
            state: Mutex::new(ConnectionState::default()),
            delivery_gate: Mutex::new(()),
            foreground_deliveries: Arc::new(std::sync::Mutex::new(RapidHashMap::new())),
            autonomous_delivery: Mutex::new(RapidHashMap::new()),
            observed: Mutex::new(BTreeMap::new()),
            observers: Mutex::new(RapidHashMap::new()),
            #[cfg(test)]
            insertion_gate: Mutex::new(None),
            #[cfg(test)]
            disconnect_gate: Mutex::new(None),
            #[cfg(test)]
            cancel_drain_gate: Mutex::new(None),
            #[cfg(test)]
            cancel_selection_gate: Mutex::new(None),
        }
    }

    pub(super) async fn begin_foreground_delivery(&self, session_id: &str) -> ForegroundDelivery {
        let _delivery = self.delivery_gate.lock().await;
        *self
            .foreground_deliveries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(session_id.to_owned())
            .or_default() += 1;
        ForegroundDelivery {
            sessions: self.foreground_deliveries.clone(),
            session_id: session_id.to_owned(),
        }
    }

    pub(super) async fn observe_session(
        self: &Arc<Self>,
        session_id: &str,
        sender: PromptSender,
        projections: Arc<ProjectionEngine>,
    ) {
        self.observe_session_since(
            session_id,
            chrono::Utc::now().to_rfc3339(),
            sender,
            projections,
        )
        .await;
    }

    pub(super) async fn observe_session_since(
        self: &Arc<Self>,
        session_id: &str,
        attached_at: String,
        sender: PromptSender,
        projections: Arc<ProjectionEngine>,
    ) {
        let mut observers = self.observers.lock().await;
        if observers.contains_key(session_id) || self.state.lock().await.closed {
            return;
        }
        let session_id = session_id.to_owned();
        let manager = Arc::downgrade(self);
        let observed_session = session_id.clone();
        let mut changes = subscribe_projected_changes(&self.node);
        observers.insert(
            session_id,
            tokio::spawn(async move {
                let mut first_pass = true;
                let mut after = String::new();
                let mut delivery_after = String::new();
                let mut goal_cursor = super::goals::GoalCursor::default();
                let mut graph_cursor = super::graph_runs::GraphRunCursor::default();
                let mut job_cursor = super::job_runs::JobRunCursor::default();
                let mut goal_refresh_at = tokio::time::Instant::now();
                loop {
                    if !std::mem::take(&mut first_pass) {
                        wait_for_projected_change(&mut changes).await;
                    }
                    let Some(manager) = manager.upgrade() else {
                        break;
                    };
                    if manager.state.lock().await.closed {
                        break;
                    }
                    if tokio::time::Instant::now() >= goal_refresh_at {
                        goal_refresh_at = tokio::time::Instant::now() + Duration::from_secs(1);
                        if let Err(error) = goal_cursor
                            .refresh(
                                &manager.node,
                                &manager.config.node_did,
                                &observed_session,
                                &sender,
                                &projections,
                            )
                            .await
                        {
                            tracing::warn!(%error, session_id = %observed_session,
                            "Grok goal observation failed; retrying");
                        }
                        for error in graph_cursor
                            .refresh(
                                &manager.node,
                                &manager.config.node_did,
                                &observed_session,
                                &sender,
                                &projections,
                            )
                            .await
                        {
                            tracing::warn!(error = %format_args!("{error:#}"),
                            session_id = %observed_session, "Grok graph run observation failed");
                        }
                        for error in job_cursor
                            .refresh(
                                &manager.node,
                                &manager.config.node_did,
                                &observed_session,
                                &sender,
                                &projections,
                            )
                            .await
                        {
                            tracing::warn!(error = %format_args!("{error:#}"),
                            session_id = %observed_session, "Grok job observation failed");
                        }
                    }
                    if let Err(error) = manager
                        .observe_session_tick(
                            &observed_session,
                            &attached_at,
                            &mut after,
                            &mut delivery_after,
                            &sender,
                            &projections,
                        )
                        .await
                    {
                        tracing::warn!(%error, session_id = %observed_session,
                        "Grok shim session observation failed; retrying");
                    }
                }
            }),
        );
    }

    pub(super) async fn replay_session(
        &self,
        session_id: &str,
        rows: &[gents_protocol::row::AgentRequestRow],
        sender: &PromptSender,
        projections: &ProjectionEngine,
    ) -> Result<Option<String>> {
        let replay = PromptSender::Replay {
            inner: Arc::new(sender.clone()),
        };
        let mut running = None;
        for row in rows {
            let started_at = row
                .created_at
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|time| time.timestamp_millis())
                .unwrap_or(0);
            let live_prompt_id = format!("notifications-{}", row.request_id);
            let prompt_id = if row.runtime_source_kind.is_some() {
                live_prompt_id.clone()
            } else {
                row.request_id.clone()
            };
            let mut progress = ObservedRequest::new(live_prompt_id.clone(), started_at, false);
            progress.cursor.lock().await.request = Some(row.clone());
            if let Some(content) = row.content.as_deref().filter(|value| !value.is_empty()) {
                projections.session_updates().send(session_id, |event_id, total_tokens| {
                    Ok(super::projection::session_notification_for_method(SESSION_UPDATE_METHOD, session_id,
                        json!({"sessionUpdate":"user_message_chunk", "content":{"type":"text", "text":content},
                            "_meta":{"hideFromScrollback":row.runtime_source_kind.is_some()}}),
                        super::projection::stamp_update_meta(event_id, total_tokens, Some(&prompt_id), Some(true),
                            UpdateTimestamps { agent_timestamp_ms: Some(started_at), ..Default::default() }),
                    ))
                }, PromptSenderLine(&replay)).await?;
            }
            let terminal = self.request_stop_reason(row).await?;
            self.stream_projection_updates(
                session_id,
                &row.request_id,
                Some(&prompt_id),
                &replay,
                projections,
                &progress.cursor,
                &mut progress.timing,
                true,
                0,
            )
            .await?;
            if let Some(reason) = terminal {
                projections.session_updates().send(session_id, |event_id, total_tokens| {
                    Ok(super::projection::session_notification_for_method("x.ai/session_notification", session_id,
                        json!({"sessionUpdate":"turn_completed", "prompt_id":prompt_id, "stop_reason":reason.wire_name()}),
                        super::projection::stamp_update_meta(event_id, total_tokens, Some(&prompt_id), Some(true), UpdateTimestamps::default()),
                    ))
                }, PromptSenderLine(&replay)).await?;
                progress.completion_sent = true;
                progress.prompt_id = None;
            } else if running.is_none() {
                running = Some((row.request_id.clone(), live_prompt_id));
            }
            self.observed.lock().await.insert(
                (session_id.to_owned(), row.request_id.clone()),
                Arc::new(Mutex::new(progress)),
            );
        }
        if let Some((request_id, _)) = &running {
            self.autonomous_delivery
                .lock()
                .await
                .insert(session_id.to_owned(), request_id.clone());
        }
        Ok(running.map(|(_, prompt)| prompt))
    }

    async fn observe_session_tick(
        &self,
        session_id: &str,
        attached_at: &str,
        after: &mut String,
        delivery_after: &mut String,
        sender: &PromptSender,
        projections: &ProjectionEngine,
    ) -> Result<()> {
        const PAGE_SIZE: usize = 128;
        let attached_at = chrono::DateTime::parse_from_rfc3339(attached_at)?
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let node = escape_graphql_string(&self.config.node_did);
        let scope = gents::session::public_request_filter(&format!(
            r#"session_id: {{ _eq: "{}" }}, node_did: {{ _eq: "{node}" }}, requester_did: {{ _eq: "{node}" }}, created_at: {{ _gte: "{}" }}, request_id: {{ _gt: "{}" }}"#,
            escape_graphql_string(session_id),
            escape_graphql_string(&attached_at),
            escape_graphql_string(after),
        ));
        let query = format!(
            r#"{{ AgentRequest(filter: {{ {scope} }},
                order: [{{ request_id: ASC }}, {{ _docID: ASC }}], limit: {PAGE_SIZE}) {{ {fields} }} }}"#,
            fields = gents::SIGNED_REQUEST_FIELDS,
        );
        let response = graphql_with_transaction_retry(
            &self.node,
            &query,
            "Grok shim session request discovery",
        )
        .await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .context("missing session request discovery rows")?;
        for row in rows {
            let request: gents_protocol::row::AgentRequestRow =
                serde_json::from_value(row.clone())?;
            anyhow::ensure!(
                request.node_did.as_deref() == Some(self.config.node_did.as_str())
                    && request.requester_did.as_deref() == Some(self.config.node_did.as_str())
                    && request.session_id.as_deref() == Some(session_id)
                    && request.doc_id.as_deref().is_some_and(|id| !id.is_empty()),
                "observed request query crossed physical scope"
            );
            let request_id = row
                .get("request_id")
                .and_then(Value::as_str)
                .context("session request missing request_id")?;
            let state = self.state.lock().await;
            if state.entries.iter().any(|((session, _), pending)| {
                session == session_id
                    && pending
                        .request
                        .as_ref()
                        .map(|row| row.request_id.as_str())
                        .is_none_or(|id| id == request_id)
            }) {
                continue;
            }
            let started_at = row
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.timestamp_millis())
                .unwrap_or(0);
            self.observed
                .lock()
                .await
                .entry((session_id.to_owned(), request_id.to_owned()))
                .or_insert_with(|| {
                    let mut progress = ObservedRequest::new(
                        format!("notifications-{request_id}"),
                        started_at,
                        false,
                    );
                    progress.echo_sent = false;
                    progress.cursor.get_mut().request = Some(request);
                    Arc::new(Mutex::new(progress))
                });
        }
        *after = if rows.len() == PAGE_SIZE {
            rows.last()
                .and_then(|row| row.get("request_id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        } else {
            String::new()
        };

        const DELIVERY_PAGE_SIZE: usize = 32;
        let mut candidates: Vec<_> = self
            .observed
            .lock()
            .await
            .iter()
            .filter(|((session, _), _)| session == session_id)
            .map(|((_, id), progress)| {
                let autonomous = progress
                    .try_lock()
                    .map(|p| p.prompt_id.is_some())
                    .unwrap_or(false);
                (
                    format!("{}:{id}", u8::from(autonomous)),
                    id.clone(),
                    progress.clone(),
                )
            })
            .collect();
        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        candidates.retain(|(key, _, _)| key > &*delivery_after);
        candidates.truncate(DELIVERY_PAGE_SIZE);
        *delivery_after = if candidates.len() == DELIVERY_PAGE_SIZE {
            candidates
                .last()
                .map(|(key, _, _)| key.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut requests: Vec<_> = candidates
            .into_iter()
            .map(|(_, id, progress)| (id, progress))
            .collect();
        if let Some(active) = self
            .autonomous_delivery
            .lock()
            .await
            .get(session_id)
            .cloned()
        {
            if !requests.iter().any(|(id, _)| id == &active) {
                if let Some(progress) = self
                    .observed
                    .lock()
                    .await
                    .get(&(session_id.to_owned(), active.clone()))
                    .cloned()
                {
                    requests.push((active, progress));
                }
            }
        }
        for (request_id, progress) in requests {
            let _delivery = self.delivery_gate.lock().await;
            let foreground_busy = self
                .foreground_deliveries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(session_id);
            let Ok(mut progress) = progress.try_lock() else {
                continue;
            };
            let request_busy = {
                let state = self.state.lock().await;
                if state.closed {
                    continue;
                }
                state
                    .entries
                    .keys()
                    .any(|(session, _)| session == session_id)
            };
            let active = self
                .autonomous_delivery
                .lock()
                .await
                .get(session_id)
                .cloned();
            if foreground_busy
                || request_busy
                || active.as_deref().is_some_and(|id| id != request_id)
            {
                let ObservedRequest {
                    prompt_id,
                    cursor,
                    timing,
                    ..
                } = &mut *progress;
                self.stream_projection_updates_mode(
                    session_id,
                    &request_id,
                    prompt_id.as_deref(),
                    sender,
                    projections,
                    cursor,
                    timing,
                    false,
                    0,
                    true,
                )
                .await?;
                continue;
            }
            let continuing_autonomous = active.is_some();
            if progress.prompt_id.is_some() && !continuing_autonomous {
                self.autonomous_delivery
                    .lock()
                    .await
                    .insert(session_id.to_owned(), request_id.clone());
            }
            if !progress.echo_sent {
                if let Some((prompt_id, content)) =
                    self.observed_human_prompt(session_id, &request_id).await?
                {
                    projections.session_updates().send(session_id, |event_id, total_tokens| {
                        Ok(super::projection::session_notification_for_method(SESSION_UPDATE_METHOD, session_id,
                            json!({"sessionUpdate":"user_message_chunk", "content":{"type":"text","text":content},
                                "_meta":{"hideFromScrollback":false,"promptIndex":0}}),
                            super::projection::stamp_update_meta(event_id, total_tokens, Some(&prompt_id), Some(false),
                                UpdateTimestamps { agent_timestamp_ms:Some(progress.timing.turn_start_ms),
                                    turn_start_ms:Some(progress.timing.turn_start_ms), ..Default::default() }),
                        ))
                    }, PromptSenderLine(sender)).await?;
                    progress.prompt_id = Some(prompt_id);
                }
                progress.echo_sent = true;
            }
            let projected = self
                .pinned_projection_request(&progress.cursor, session_id, &request_id, true)
                .await?;
            let terminal = self.request_stop_reason(&projected).await?;
            let ObservedRequest {
                prompt_id,
                cursor,
                timing,
                ..
            } = &mut *progress;
            self.stream_projection_updates(
                session_id,
                &request_id,
                prompt_id.as_deref(),
                sender,
                projections,
                cursor,
                timing,
                !continuing_autonomous,
                0,
            )
            .await?;
            if !progress.completion_sent {
                if let Some(reason) = terminal {
                    let prompt_id = progress
                        .prompt_id
                        .clone()
                        .context("missing autonomous prompt ID")?;
                    projections.session_updates().send(session_id, move |event_id, total_tokens| {
                        Ok(super::projection::session_notification_for_method(
                            "x.ai/session_notification", session_id,
                            json!({"sessionUpdate": "turn_completed", "prompt_id": prompt_id,
                                "stop_reason": reason.wire_name()}),
                            super::projection::stamp_update_meta(event_id, total_tokens,
                                Some(&prompt_id), None, UpdateTimestamps::default()),
                        ))
                    }, PromptSenderLine(sender)).await?;
                    progress.completion_sent = true;
                    progress.prompt_id = None;
                    self.autonomous_delivery.lock().await.remove(session_id);
                    delivery_after.clear();
                    break;
                }
            }
        }
        Ok(())
    }

    pub(super) async fn handle_prompt(
        &self,
        request: PromptRequest,
        sender: &PromptSender,
        projections: &ProjectionEngine,
    ) -> Result<Value> {
        if request.prompt.is_empty() {
            anyhow::bail!("session/prompt requires at least one prompt block");
        }
        let turn_start_ms = chrono::Utc::now().timestamp_millis();
        let prompt_id = request
            .prompt_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let key = (request.session_id.clone(), prompt_id.clone());
        let (response_tx, response_rx) = oneshot::channel::<Result<Value>>();
        let cancel_before_id = Arc::new(Mutex::new(CancelBeforeIdLatch::default()));
        #[cfg(test)]
        {
            let gate = self.insertion_gate.lock().await.clone();
            if let Some(gate) = gate {
                gate.arrived.notify_one();
                gate.release.notified().await;
            }
        }
        {
            let _admission = self.delivery_gate.lock().await;
            let mut state = self.state.lock().await;
            if state.is_closed() {
                anyhow::bail!("connection already disconnected");
            }
            if state
                .entries
                .keys()
                .any(|(session, _)| session == &request.session_id)
            {
                anyhow::bail!("session already has a live prompt");
            }
            state.entries.insert(
                key.clone(),
                PendingPrompt {
                    response_tx: Some(response_tx),
                    request: None,
                    cancel_before_id: cancel_before_id.clone(),
                    drained: false,
                },
            );
        }

        tracing::debug!(session_id = %request.session_id, prompt_id = %prompt_id, "Grok shim submitting prompt request");
        let progress = Arc::new(Mutex::new(ObservedRequest::new(
            prompt_id.clone(),
            turn_start_ms,
            true,
        )));
        let mut progress_guard = progress.lock().await;
        let submission = self.submit_request(&request, &prompt_id).await;
        let (request_id, submitted_request) = match submission {
            Ok(row) => {
                let request_id = row.request_id.clone();
                progress_guard.cursor.lock().await.request = Some(row.clone());
                let registered_own_entry = {
                    let mut state = self.state.lock().await;
                    match state.entries.get_mut(&key) {
                        Some(entry) if Arc::ptr_eq(&entry.cancel_before_id, &cancel_before_id) => {
                            entry.request = Some(row.clone());
                            self.observed.lock().await.insert(
                                (request.session_id.clone(), request_id.clone()),
                                progress.clone(),
                            );
                            true
                        }
                        _ => false,
                    }
                };
                let cancelled_before_id = cancel_before_id.lock().await.is_cancelled();
                if !registered_own_entry || cancelled_before_id {
                    self.interrupt_submitted(&row).await;
                    drop(response_rx);
                    tracing::info!(
                        session_id = %request.session_id,
                        prompt_id = %prompt_id,
                        request_id = %request_id,
                        registered_own_entry,
                        cancelled_before_id,
                        "Grok shim prompt was drained during submission; interrupting submitted request"
                    );
                    return Ok(json!({"stopReason": StopReason::Cancelled.wire_name()}));
                }
                (request_id, row)
            }
            Err(error) => {
                let (own_entry, state_closed) =
                    self.take_entry_if_generation(&key, &cancel_before_id).await;
                let own_entry_was_present = own_entry.is_some();
                let cancelled_by_latch = cancel_before_id.lock().await.is_cancelled();
                let cancelled_by_disconnect = state_closed && !own_entry_was_present;
                let cancelled_by_drain = !own_entry_was_present;
                drop(response_rx);
                if cancelled_by_disconnect || cancelled_by_drain || cancelled_by_latch {
                    tracing::info!(
                        %error,
                        session_id = %request.session_id,
                        prompt_id = %prompt_id,
                        state_closed,
                        own_entry_was_present,
                        cancelled_by_latch,
                        "Grok shim prompt submission failed into a cancelled/disconnected turn; resolving cancelled"
                    );
                    return Ok(json!({"stopReason": StopReason::Cancelled.wire_name()}));
                }
                tracing::warn!(
                    %error,
                    session_id = %request.session_id,
                    prompt_id = %prompt_id,
                    "Grok shim prompt submission failed"
                );
                return Err(error);
            }
        };

        let drained_during_submission = {
            let state = self.state.lock().await;
            state.entries.get(&key).is_none_or(|entry| {
                !Arc::ptr_eq(&entry.cancel_before_id, &cancel_before_id) || entry.drained
            })
        };
        if drained_during_submission {
            self.interrupt_submitted(&submitted_request).await;
            drop(response_rx);
            tracing::info!(
                session_id = %request.session_id,
                prompt_id = %prompt_id,
                request_id = %request_id,
                "Grok shim prompt drained before user echo; interrupting submitted request"
            );
            return Ok(json!({"stopReason": StopReason::Cancelled.wire_name()}));
        }
        for (index, block) in request.prompt.iter().enumerate() {
            if let Err(error) = sender
                .send_user_message_chunk(
                    projections.session_updates(),
                    &request.session_id,
                    &prompt_id,
                    block,
                    index,
                )
                .await
            {
                self.interrupt_and_drain(&key, &submitted_request, &cancel_before_id)
                    .await;
                drop(response_rx);
                tracing::warn!(
                    %error,
                    session_id = %request.session_id,
                    prompt_id = %prompt_id,
                    request_id = %request_id,
                    "Grok shim user echo send failed; interrupted submitted request"
                );
                return Err(error);
            }
        }

        let outcome = self
            .watch_terminal(
                &key,
                &submitted_request,
                &request.session_id,
                &prompt_id,
                &cancel_before_id,
                sender,
                projections,
                &mut progress_guard,
                response_rx,
            )
            .await;
        let outcome = outcome?;
        prompt_result(outcome)
    }

    async fn observed_human_prompt(
        &self,
        session: &str,
        request: &str,
    ) -> Result<Option<(String, String)>> {
        let node = escape_graphql_string(&self.config.node_did);
        let scope = gents::session::public_request_filter(&format!(
            r#"request_id:{{_eq:"{}"}},session_id:{{_eq:"{}"}},node_did:{{_eq:"{node}"}},requester_did:{{_eq:"{node}"}}"#,
            escape_graphql_string(request),
            escape_graphql_string(session)
        ));
        let query =
            format!(r#"{{AgentRequest(filter:{{{scope}}}) {{content runtime_source_kind}}}}"#,);
        let response =
            graphql_with_transaction_retry(&self.node, &query, "read observed human prompt")
                .await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data["AgentRequest"].as_array())
            .context("missing observed request rows")?;
        anyhow::ensure!(
            rows.len() == 1,
            "observed request ownership is missing or ambiguous"
        );
        let row = &rows[0];
        if row["runtime_source_kind"].as_str().is_some() {
            return Ok(None);
        }
        Ok(Some((
            request.to_owned(),
            row["content"].as_str().unwrap_or_default().to_owned(),
        )))
    }

    pub(super) async fn handle_cancel(&self, notification: CancelNotification) -> Result<()> {
        tracing::info!(
            session_id = %notification.session_id,
            prompt_id = notification.prompt_id.as_deref().unwrap_or(""),
            "Grok shim received session/cancel"
        );
        let target_prompt_id = notification.prompt_id.clone();
        let targets: Vec<(PromptKey, CancelLatch)> = {
            let state = self.state.lock().await;
            state
                .entries
                .iter()
                .filter(|((session, _), _)| session == &notification.session_id)
                .filter(|((_, prompt_id), _)| {
                    target_prompt_id
                        .as_deref()
                        .is_none_or(|expected| expected == prompt_id.as_str())
                })
                .map(|(key, entry)| (key.clone(), entry.cancel_before_id.clone()))
                .collect()
        };
        #[cfg(test)]
        {
            let gate = self.cancel_selection_gate.lock().await.clone();
            if let Some(gate) = gate {
                gate.arrived.notify_one();
                gate.release.notified().await;
            }
        }
        let had_foreground_target = !targets.is_empty();
        for (key, generation) in targets {
            let request = self
                .drain_entry(&key, &generation, StopReason::Cancelled)
                .await;
            if let Some(request) = request {
                self.interrupt_submitted(&request).await;
            }
        }
        let autonomous_target = match target_prompt_id.as_deref() {
            Some(id) if id.starts_with("notifications-") => {
                id.strip_prefix("notifications-").map(str::to_owned)
            }
            Some(id) => {
                let active = self
                    .autonomous_delivery
                    .lock()
                    .await
                    .get(&notification.session_id)
                    .cloned();
                match active {
                    Some(request)
                        if self
                            .observed_human_prompt(&notification.session_id, &request)
                            .await?
                            .is_some_and(|(prompt, _)| prompt == id) =>
                    {
                        Some(request)
                    }
                    _ => None,
                }
            }
            None if !had_foreground_target => self
                .autonomous_delivery
                .lock()
                .await
                .get(&notification.session_id)
                .cloned(),
            None => None,
        };
        if let Some(request_id) = autonomous_target.as_deref() {
            let observed = self
                .observed
                .lock()
                .await
                .contains_key(&(notification.session_id.clone(), request_id.to_owned()));
            let row = if observed {
                Some(
                    self.load_projection_request(
                        &self.config.node_did,
                        &notification.session_id,
                        Some(&self.config.node_did),
                        request_id,
                        None,
                    )
                    .await?,
                )
            } else {
                None
            };
            if let Some(row) = row.filter(|_| observed) {
                if self.request_stop_reason(&row).await?.is_some() {
                    return Ok(());
                }
                self.interrupt_submitted(&row).await;
            }
        }
        Ok(())
    }

    pub(super) async fn handle_disconnect(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        state.closed = true;
        let drained: Vec<(
            PromptKey,
            CancelLatch,
            Option<gents_protocol::row::AgentRequestRow>,
        )> = std::mem::take(&mut state.entries)
            .into_iter()
            .map(|(key, mut entry)| {
                entry.resolve(Ok(json!({
                    "stopReason": StopReason::Cancelled.wire_name(),
                })));
                (key, entry.cancel_before_id.clone(), entry.request.clone())
            })
            .collect();
        drop(state);
        let observers = std::mem::take(&mut *self.observers.lock().await);
        for observer in observers.values() {
            observer.abort();
        }
        for (_, observer) in observers {
            let _ = observer.await;
        }
        #[cfg(test)]
        {
            let gate = self.disconnect_gate.lock().await.clone();
            if let Some(gate) = gate {
                gate.arrived.notify_one();
                gate.release.notified().await;
            }
        }
        for ((_session_id, _prompt_id), latch, request) in drained {
            let _first_cancel = latch.lock().await.cancel();
            if let Some(request) = request {
                self.interrupt_submitted(&request).await;
            }
        }
        Ok(())
    }

    async fn drain_entry(
        &self,
        key: &(String, String),
        generation: &Arc<Mutex<CancelBeforeIdLatch>>,
        stop_reason: StopReason,
    ) -> Option<gents_protocol::row::AgentRequestRow> {
        let (entry, _) = self.take_entry_if_generation(key, generation).await;
        let mut entry = entry?;
        #[cfg(test)]
        {
            let gate = self.cancel_drain_gate.lock().await.clone();
            if let Some(gate) = gate {
                gate.arrived.notify_one();
                gate.release.notified().await;
            }
        }
        let _first_cancel = entry.cancel_before_id.lock().await.cancel();
        entry.resolve(Ok(json!({
            "stopReason": stop_reason.wire_name(),
        })));
        entry.request.clone()
    }

    async fn interrupt_and_drain(
        &self,
        key: &(String, String),
        request: &gents_protocol::row::AgentRequestRow,
        generation: &Arc<Mutex<CancelBeforeIdLatch>>,
    ) {
        if let (Some(mut entry), _) = self.take_entry_if_generation(key, generation).await {
            let _first_cancel = entry.cancel_before_id.lock().await.cancel();
            entry.resolve(Ok(json!({
                "stopReason": StopReason::Cancelled.wire_name(),
            })));
        }
        self.interrupt_submitted(request).await;
    }

    async fn take_entry_if_generation(
        &self,
        key: &(String, String),
        generation: &Arc<Mutex<CancelBeforeIdLatch>>,
    ) -> (Option<PendingPrompt>, bool) {
        let mut state = self.state.lock().await;
        let owns_current_generation = state
            .entries
            .get(key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.cancel_before_id, generation));
        let entry = if owns_current_generation {
            state.entries.remove(key)
        } else {
            None
        };
        (entry, state.is_closed())
    }

    async fn interrupt_submitted(&self, request: &gents_protocol::row::AgentRequestRow) {
        let Some(physical) = request.doc_id.as_deref() else {
            tracing::error!(request_id=%request.request_id,"submitted request has no physical identity");
            return;
        };
        let Some(owner) = request.node_did.as_deref() else {
            tracing::error!(request_id=%request.request_id,"submitted request has no node identity");
            return;
        };
        if let Err(error) = gents::interrupt_request_by_doc_id(
            &self.node,
            physical,
            owner,
            request.requester_did.as_deref(),
        )
        .await
        {
            tracing::warn!(%error,request_id=%request.request_id,"Grok shim failed to interrupt submitted request");
        }
    }

    async fn submit_request(
        &self,
        request: &PromptRequest,
        _prompt_id: &str,
    ) -> Result<gents_protocol::row::AgentRequestRow> {
        let content = prompt_text(request);
        let graphql = gents::config_client::GraphqlEndpoint::as_principal(
            self.config.graphql.clone(),
            self.config.actor.to_string(),
        );
        let stable_request_id = uuid::Uuid::new_v4().to_string();
        let options = crate::RequestSubmitOptions::default();
        let submitted = if let Some(super::goals::GoalCommand::Create {
            objective,
            token_budget,
        }) = super::goals::GoalCommand::from_prompt(request)?
        {
            crate::create_goal_backed_agent_request_local(
                &self.node,
                self.config.actor.clone(),
                &graphql,
                &self.config.node_did,
                &objective,
                token_budget,
                &request.session_id,
                &self.config.agent_id,
                stable_request_id.clone(),
                options,
            )
            .await
        } else {
            // The shim owns the destination node, so it binds the canonical
            // Goal directly; the signed remote observation exists for callers
            // that cannot read it, and lags a session the shim just opened.
            let mut options = options;
            let (node_did, agent_id) = (&*self.config.node_did, &*self.config.agent_id);
            let session_id = &*request.session_id;
            options.input = Some(
                gents::ConfigAccess::local(self.node.clone())
                    .transact_readonly("grok.prompt.goal_input", |txn| {
                        Box::pin(gents::goal::prepare_goal_input_in_txn(
                            txn,
                            node_did,
                            session_id,
                            agent_id,
                            Some(node_did),
                            Default::default(),
                        ))
                    })
                    .await?,
            );
            let prepared = crate::request_helpers::prepare_agent_request_with_input(
                &graphql,
                &self.config.node_did,
                &content,
                Some(&request.session_id),
                Some(&self.config.agent_id),
                Some(stable_request_id.clone()),
                options,
            )
            .await?;
            let result = crate::request_helpers::submit_prepared_agent_request_committed(
                &graphql, &prepared,
            )
            .await;
            if result.is_err() {
                if let Ok(Some(row)) =
                    crate::request_helpers::matching_prepared_receipt(&graphql, &prepared.create)
                        .await
                {
                    self.interrupt_submitted(&row).await;
                }
            }
            result
        };
        let submitted = submitted?;
        let row = self
            .load_projection_request(
                &submitted.node_did,
                &submitted.session_id,
                submitted.requester_did.as_deref(),
                &submitted.request_id,
                Some(&submitted.request_doc_id),
            )
            .await;
        if row.is_err() {
            if let Err(error) = gents::interrupt_request_by_doc_id(
                &self.node,
                &submitted.request_doc_id,
                &submitted.node_did,
                submitted.requester_did.as_deref(),
            )
            .await
            {
                tracing::warn!(%error,request_id=%submitted.request_id,"failed to interrupt committed request after projection read failure");
            }
        }
        row
    }

    async fn load_projection_request(
        &self,
        agent: &str,
        session: &str,
        requester: Option<&str>,
        request: &str,
        physical: Option<&str>,
    ) -> Result<gents_protocol::row::AgentRequestRow> {
        let physical_filter = physical
            .map(|id| format!("_docID: {{_eq: \"{}\"}},", escape_graphql_string(id)))
            .unwrap_or_default();
        let query = format!(
            "{{AgentRequest(filter: {{{physical_filter} {}, request_id: {{_eq: \"{}\"}}}},limit:2) {{{}}}}}",
            gents::session::public_request_filter(&gents::session::session_scope_filter(agent, session, requester)),
            escape_graphql_string(request),
            gents::SIGNED_REQUEST_FIELDS
        );
        let response =
            graphql_with_transaction_retry(&self.node, &query, "load scoped projection request")
                .await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .context("projection request query returned no rows")?;
        let [row] = rows.as_slice() else {
            anyhow::bail!("projection request missing or ambiguous");
        };
        let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone())?;
        anyhow::ensure!(
            row.request_id == request
                && row.node_did.as_deref() == Some(agent)
                && row.session_id.as_deref() == Some(session)
                && row.requester_did.as_deref() == requester
                && row.doc_id.as_deref().is_some_and(
                    |id| !id.is_empty() && physical.is_none_or(|expected| expected == id)
                ),
            "projection request query crossed exact physical scope"
        );
        Ok(row)
    }

    #[allow(clippy::too_many_arguments)]
    async fn watch_terminal(
        &self,
        key: &(String, String),
        request: &gents_protocol::row::AgentRequestRow,
        session_id: &str,
        prompt_id: &str,
        generation: &Arc<Mutex<CancelBeforeIdLatch>>,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        progress: &mut ObservedRequest,
        mut response_rx: oneshot::Receiver<Result<Value>>,
    ) -> Result<TerminalOutcome> {
        let request_id = request.request_id.as_str();
        let ObservedRequest {
            cursor,
            timing: update_timing,
            ..
        } = progress;
        let mut consecutive_transient_read_failures = 0usize;
        let mut changes = subscribe_projected_changes(&self.node);
        loop {
            if let Ok(result) = response_rx.try_recv() {
                let value = result?;
                let stop_reason = value
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or(StopReason::Cancelled.wire_name());
                return Ok(stop_reason_from_wire(stop_reason).into());
            }
            let projected = self
                .pinned_projection_request(cursor, session_id, request_id, false)
                .await?;
            let terminal = match self.request_terminal(&projected).await {
                Ok(terminal) => terminal,
                Err(error)
                    if register_transient_read_retry(
                        &error,
                        &mut consecutive_transient_read_failures,
                    ) =>
                {
                    tracing::warn!(
                        %error,
                        session_id,
                        prompt_id,
                        request_id,
                        retry = consecutive_transient_read_failures,
                        "retrying transient Grok shim terminal query failure"
                    );
                    if let Some(stop_reason) = wait_for_retry_or_cancel(
                        &mut response_rx,
                        transient_graphql_read_retry_delay(consecutive_transient_read_failures),
                    )
                    .await?
                    {
                        return Ok(stop_reason.into());
                    }
                    continue;
                }
                Err(error) => {
                    self.interrupt_and_drain(key, request, generation).await;
                    drop(response_rx);
                    tracing::warn!(
                        %error,
                        session_id,
                        prompt_id,
                        request_id,
                        "Grok shim terminal query failed; interrupted submitted request"
                    );
                    return Err(error);
                }
            };
            match self
                .stream_projection_updates(
                    session_id,
                    request_id,
                    Some(prompt_id),
                    sender,
                    projections,
                    cursor,
                    update_timing,
                    false,
                    0,
                )
                .await
            {
                Ok(()) => {}
                Err(error)
                    if register_transient_read_retry(
                        &error,
                        &mut consecutive_transient_read_failures,
                    ) =>
                {
                    tracing::warn!(
                        %error,
                        session_id,
                        prompt_id,
                        request_id,
                        retry = consecutive_transient_read_failures,
                        "retrying transient Grok shim projection read failure"
                    );
                    if let Some(stop_reason) = wait_for_retry_or_cancel(
                        &mut response_rx,
                        transient_graphql_read_retry_delay(consecutive_transient_read_failures),
                    )
                    .await?
                    {
                        return Ok(stop_reason.into());
                    }
                    continue;
                }
                Err(error) => {
                    self.interrupt_and_drain(key, request, generation).await;
                    drop(response_rx);
                    tracing::warn!(
                        %error,
                        session_id,
                        prompt_id,
                        request_id,
                        "Grok shim live projection failed; interrupted submitted request"
                    );
                    return Err(error);
                }
            }
            consecutive_transient_read_failures = 0;
            if let Some(outcome) = terminal {
                self.take_entry_if_generation(key, generation).await;
                return Ok(outcome);
            }
            tokio::select! {
                () = wait_for_projected_change(&mut changes) => {}
                result = &mut response_rx => {
                    if let Ok(result) = result {
                        let value = result?;
                        let stop_reason = value
                            .get("stopReason")
                            .and_then(Value::as_str)
                            .unwrap_or(StopReason::Cancelled.wire_name());
                        return Ok(stop_reason_from_wire(stop_reason).into());
                    }
                    return Ok(StopReason::Cancelled.into());
                }
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "per-prompt stream state borrowed from separate owners"
    )]
    async fn stream_projection_updates(
        &self,
        session_id: &str,
        request_id: &str,
        prompt_id: Option<&str>,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        cursor: &tokio::sync::Mutex<RequestCursor>,
        update_timing: &mut RequestUpdateTiming,
        include_notifications: bool,
        child_depth: usize,
    ) -> Result<()> {
        self.stream_projection_updates_mode(
            session_id,
            request_id,
            prompt_id,
            sender,
            projections,
            cursor,
            update_timing,
            include_notifications,
            child_depth,
            false,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "per-prompt stream state borrowed from separate owners"
    )]
    async fn stream_projection_updates_mode(
        &self,
        session_id: &str,
        request_id: &str,
        prompt_id: Option<&str>,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        cursor: &tokio::sync::Mutex<RequestCursor>,
        update_timing: &mut RequestUpdateTiming,
        include_notifications: bool,
        child_depth: usize,
        activity_only: bool,
    ) -> Result<()> {
        let prompt_id = if activity_only { None } else { prompt_id };
        let request = self
            .pinned_projection_request(cursor, session_id, request_id, child_depth == 0)
            .await?;
        let batch = {
            let mut cursor = cursor.lock().await;
            projections
                .project_request_updates(&request, &mut cursor, prompt_id)
                .await?
        };
        let mut deferred_notifications = false;
        let mut child_finishes = Vec::new();
        for event in batch.events {
            let event = if child_depth > 0 {
                cursor.lock().await.child_output_event(event)
            } else {
                event
            };
            if activity_only && !is_background_activity(&event.payload) {
                continue;
            }
            if event.payload.get("sessionUpdate").and_then(Value::as_str)
                == Some("subagent_finished")
            {
                child_finishes.push(event);
                continue;
            }
            if !include_notifications
                && event.payload.get("sessionUpdate").and_then(Value::as_str)
                    == Some("user_message_chunk")
            {
                deferred_notifications = true;
                continue;
            }
            self.send_projection_event(
                session_id,
                prompt_id,
                event,
                sender,
                projections,
                cursor,
                update_timing,
                activity_only,
            )
            .await?;
        }
        let deferred_children = self
            .stream_readable_child_updates(session_id, sender, projections, cursor, child_depth)
            .await?;
        for event in child_finishes {
            if event
                .payload
                .get("child_session_id")
                .and_then(Value::as_str)
                .is_some_and(|session| deferred_children.contains(session))
            {
                continue;
            }
            self.send_projection_event(
                session_id,
                prompt_id,
                event,
                sender,
                projections,
                cursor,
                update_timing,
                activity_only,
            )
            .await?;
        }
        if !activity_only && !batch.trailing_advances.is_empty() {
            let mut cursor = cursor.lock().await;
            for advance in batch.trailing_advances {
                if deferred_notifications
                    && matches!(advance, CursorAdvance::MessageHighWater { .. })
                {
                    continue;
                }
                cursor.record(advance);
            }
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "per-prompt stream state borrowed from separate owners"
    )]
    async fn send_projection_event(
        &self,
        session_id: &str,
        prompt_id: Option<&str>,
        event: NovelProjectionEvent,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        cursor: &Mutex<RequestCursor>,
        timing: &mut RequestUpdateTiming,
        activity_only: bool,
    ) -> Result<()> {
        let timestamps = if activity_only {
            UpdateTimestamps::default()
        } else {
            timing.resolve(event.timing.as_ref(), chrono::Utc::now().timestamp_millis())
        };
        let commit = ProjectionCursorCommit {
            cursor,
            advance: event.advance,
        };
        projections
            .session_updates()
            .send_with_commit(
                session_id,
                move |event_id, total_tokens| {
                    let meta = super::projection::stamp_update_meta(
                        event_id,
                        total_tokens,
                        prompt_id,
                        None,
                        timestamps,
                    );
                    Ok(super::projection::session_notification_for_method(
                        event.method,
                        session_id,
                        event.payload,
                        meta,
                    ))
                },
                PromptSenderLine(sender),
                commit,
            )
            .await?;
        Ok(())
    }

    async fn stream_readable_child_updates(
        &self,
        parent_session_id: &str,
        sender: &PromptSender,
        projections: &ProjectionEngine,
        parent_cursor: &Mutex<RequestCursor>,
        depth: usize,
    ) -> Result<BTreeSet<String>> {
        let mut deferred = BTreeSet::new();
        let parent = parent_cursor
            .lock()
            .await
            .request
            .clone()
            .context("parent projection identity missing")?;
        for child in
            super::projection::caused_sessions::caused_by_request(&self.node, &parent).await?
        {
            let session_id = child.scope.session_id.as_str();
            if session_id == parent_session_id
                || !parent_cursor
                    .lock()
                    .await
                    .subagent_spawn_was_delivered(session_id)
            {
                continue;
            }
            for row in self.readable_child_session_requests(&child.scope).await? {
                let request_id = row.request_id.clone();
                let started_at = row
                    .created_at
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.timestamp_millis())
                    .unwrap_or(0);
                let progress = self
                    .observed
                    .lock()
                    .await
                    .entry((session_id.to_owned(), request_id.clone()))
                    .or_insert_with(|| {
                        Arc::new(Mutex::new(ObservedRequest::new(
                            request_id.clone(),
                            started_at,
                            true,
                        )))
                    })
                    .clone();
                let Ok(mut progress) = progress.try_lock() else {
                    deferred.insert(session_id.to_owned());
                    continue;
                };
                {
                    let mut cursor = progress.cursor.lock().await;
                    if let Some(pinned) = &cursor.request {
                        anyhow::ensure!(
                            pinned.doc_id == row.doc_id
                                && pinned.node_did == row.node_did
                                && pinned.requester_did == row.requester_did,
                            "child projection cannot rebind physical request"
                        );
                    } else {
                        cursor.request = Some(row);
                    }
                }
                let projected = progress
                    .cursor
                    .lock()
                    .await
                    .request
                    .clone()
                    .expect("verified child identity pinned");
                let terminal = self.request_stop_reason(&projected).await?;
                if terminal.is_none() {
                    deferred.insert(session_id.to_owned());
                }
                let ObservedRequest { cursor, timing, .. } = &mut *progress;
                Box::pin(self.stream_projection_updates(
                    session_id,
                    &request_id,
                    None,
                    sender,
                    projections,
                    cursor,
                    timing,
                    terminal.is_some() || matches!(sender, PromptSender::Replay { .. }),
                    depth + 1,
                ))
                .await?;
            }
        }
        Ok(deferred)
    }

    async fn readable_child_session_requests(
        &self,
        child: &crate::caused_sessions::SessionScope,
    ) -> Result<Vec<gents_protocol::row::AgentRequestRow>> {
        let session = child.session_id.as_str();
        let node = child.node_did.as_str();
        let scope = gents::session::public_request_filter(&gents::session::session_scope_filter(
            node,
            session,
            child.requester_did.as_deref(),
        ));
        let mut seen = RapidHashSet::new();
        let mut requests = Vec::new();
        let mut offset = 0usize;
        loop {
            let query = format!(
                "{{AgentRequest(filter: {{{scope}}}, order: {{request_id: ASC}}, limit: 128, offset: {offset}) {{{}}}}}",
                gents::SIGNED_REQUEST_FIELDS
            );
            let response = graphql_with_transaction_retry(
                &self.node,
                &query,
                "read scoped child session requests",
            )
            .await?;
            let rows = response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentRequest"))
                .and_then(Value::as_array)
                .context("child session query missing rows")?;
            anyhow::ensure!(rows.len() <= 128, "child session query exceeded page limit");
            for value in rows {
                let row: gents_protocol::row::AgentRequestRow =
                    serde_json::from_value(value.clone())?;
                anyhow::ensure!(
                    row.node_did.as_deref() == Some(node)
                        && row.session_id.as_deref() == Some(session)
                        && row.requester_did == child.requester_did
                        && row.doc_id.as_deref().is_some_and(|id| !id.is_empty())
                        && seen.insert(row.request_id.clone()),
                    "child session query crossed scope or has ambiguous request labels"
                );
                requests.push(row);
            }
            if rows.len() < 128 {
                break;
            }
            offset = offset
                .checked_add(rows.len())
                .context("child session pagination overflow")?;
        }
        requests
            .sort_by(|a, b| (&a.created_at, &a.request_id).cmp(&(&b.created_at, &b.request_id)));
        Ok(requests)
    }

    async fn pinned_projection_request(
        &self,
        cursor: &Mutex<RequestCursor>,
        session: &str,
        request_id: &str,
        allow_root_read: bool,
    ) -> Result<gents_protocol::row::AgentRequestRow> {
        let mut cursor = cursor.lock().await;
        if cursor.request.is_none() {
            anyhow::ensure!(
                allow_root_read,
                "projection requires verified physical request receipt"
            );
            cursor.request = Some(
                self.load_projection_request(
                    &self.config.node_did,
                    session,
                    Some(&self.config.node_did),
                    request_id,
                    None,
                )
                .await?,
            );
        }
        let request = cursor.request.clone().expect("projection identity pinned");
        anyhow::ensure!(
            request.request_id == request_id && request.session_id.as_deref() == Some(session),
            "projection cursor cannot change request scope"
        );
        Ok(request)
    }

    async fn request_stop_reason(
        &self,
        request: &gents_protocol::row::AgentRequestRow,
    ) -> Result<Option<StopReason>> {
        Ok(self
            .request_terminal(request)
            .await?
            .map(|outcome| outcome.stop_reason))
    }

    async fn request_terminal(
        &self,
        request: &gents_protocol::row::AgentRequestRow,
    ) -> Result<Option<TerminalOutcome>> {
        let physical = request
            .doc_id
            .as_deref()
            .context("terminal projection physical request missing")?;
        let scope = gents::session::session_scope_filter(
            request
                .node_did
                .as_deref()
                .context("terminal projection owner missing")?,
            request
                .session_id
                .as_deref()
                .context("terminal projection session missing")?,
            request.requester_did.as_deref(),
        );
        let physical = escape_graphql_string(physical);
        let query = format!(
            r#"{{
            AgentRequest(filter: {{{scope}, _docID: {{_eq:"{physical}"}}}},limit:2)
                {{request_id lifecycle_state interrupt_requested_at failure_reason}}
        }}"#
        );
        let response =
            graphql_with_transaction_retry(&self.node, &query, "grok shim turn terminal query")
                .await?;
        let request_rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .context("terminal projection request rows missing")?;
        anyhow::ensure!(
            request_rows.len() <= 1,
            "ambiguous terminal projection physical scope"
        );
        let Some(request_row) = request_rows.first() else {
            return Ok(None);
        };
        let lifecycle_state = request_row
            .get("lifecycle_state")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let interrupted_at = request_row
            .get("interrupt_requested_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let error_message = request_row
            .get("failure_reason")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        Ok(
            stop_reason_from_rows(lifecycle_state, interrupted_at.as_deref()).map(|stop_reason| {
                TerminalOutcome {
                    stop_reason,
                    error_message,
                }
            }),
        )
    }
}

async fn wait_for_retry_or_cancel(
    response_rx: &mut oneshot::Receiver<Result<Value>>,
    delay: Duration,
) -> Result<Option<StopReason>> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(None),
        result = response_rx => {
            let value = match result {
                Ok(result) => result?,
                Err(_) => return Ok(Some(StopReason::Cancelled)),
            };
            let stop_reason = value
                .get("stopReason")
                .and_then(Value::as_str)
                .unwrap_or(StopReason::Cancelled.wire_name());
            Ok(Some(stop_reason_from_wire(stop_reason)))
        }
    }
}

fn register_transient_read_retry(error: &anyhow::Error, consecutive: &mut usize) -> bool {
    if !gents_protocol::graphql::graphql_transport_error_in_chain(error.as_ref())
        || *consecutive >= MAX_TRANSIENT_GRAPHQL_READ_RETRIES
    {
        return false;
    }
    *consecutive += 1;
    true
}

fn stop_reason_from_wire(name: &str) -> StopReason {
    match name {
        "end_turn" => StopReason::EndTurn,
        "refusal" => StopReason::Refusal,
        "error" => StopReason::Error,
        _ => StopReason::Cancelled,
    }
}

fn is_background_activity(payload: &Value) -> bool {
    matches!(
        payload["sessionUpdate"].as_str(),
        Some(
            "tool_call"
                | "tool_call_update"
                | "task_backgrounded"
                | "task_completed"
                | "monitor_event"
                | "subagent_spawned"
                | "subagent_progress"
                | "subagent_finished"
        )
    )
}

pub(super) fn stop_reason_from_rows(
    lifecycle_state: &str,
    interrupt_requested_at: Option<&str>,
) -> Option<StopReason> {
    let interrupted_at_nonempty = interrupt_requested_at
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    match lifecycle_state {
        "interrupted" => Some(StopReason::Cancelled),
        "completed" => Some(StopReason::EndTurn),
        "failed" => Some(StopReason::Error),
        "superseded" | "dead" => {
            if interrupted_at_nonempty {
                Some(StopReason::Cancelled)
            } else {
                Some(StopReason::Error)
            }
        }
        _ => None,
    }
}

pub(super) fn prompt_text(request: &PromptRequest) -> String {
    request
        .prompt
        .iter()
        .map(|block| {
            if block.kind == "text" {
                block.text.clone()
            } else {
                serde_json::to_string(&json!({
                    "type": block.kind,
                    "text": block.text,
                    "meta": block.meta,
                }))
                .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn parse_prompt_request(params: &Value, id: Option<Value>) -> Result<PromptRequest> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("session/prompt requires sessionId")?
        .to_string();
    let prompt_rows = params
        .get("prompt")
        .and_then(Value::as_array)
        .context("session/prompt requires a prompt array")?;
    if prompt_rows.is_empty() {
        anyhow::bail!("session/prompt requires at least one prompt block");
    }
    let mut prompt = Vec::with_capacity(prompt_rows.len());
    for row in prompt_rows {
        let kind = row
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("text")
            .to_string();
        let text = row
            .get("text")
            .and_then(Value::as_str)
            .context("session/prompt prompt block missing text")?
            .to_string();
        let meta = row.get("meta").cloned().filter(|meta| !meta.is_null());
        prompt.push(PromptBlock { kind, text, meta });
    }
    let meta = params.get("_meta");
    let prompt_id = meta
        .and_then(|meta| meta.get("promptId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let screen_mode = meta
        .and_then(|meta| meta.get("screenMode"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    if let Some(screen_mode) = screen_mode.as_deref() {
        if !SCREEN_MODES.contains(&screen_mode) {
            anyhow::bail!("unknown screenMode {screen_mode:?}");
        }
    }
    let send_now = meta
        .and_then(|meta| meta.get("sendNow"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(PromptRequest {
        session_id,
        prompt,
        prompt_id,
        screen_mode,
        send_now,
        id,
    })
}

pub(super) fn parse_cancel_notification(params: &Value) -> Result<CancelNotification> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("session/cancel requires sessionId")?
        .to_string();
    let meta = params.get("_meta");
    let cancel_trigger = meta
        .and_then(|meta| meta.get("cancelTrigger"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let rewind_if_no_output = meta
        .and_then(|meta| meta.get("rewindIfNoOutput"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let rewind_if_pristine = meta
        .and_then(|meta| meta.get("rewindIfPristine"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let prompt_id = meta
        .and_then(|meta| meta.get("promptId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Ok(CancelNotification {
        session_id,
        cancel_trigger,
        rewind_if_no_output,
        rewind_if_pristine,
        prompt_id,
    })
}

#[cfg(test)]
mod tests {
    use super::super::projection::ProjectionSequencer;
    use super::*;
    use gents::graphql::ensure_no_errors;

    fn text_block(text: &str) -> Value {
        json!({"type": "text", "text": text})
    }

    #[test]
    fn parse_prompt_request_reads_audited_meta() {
        let params = json ! ({ "sessionId" : "session-1" , "prompt" : [{ "type" : "text" , "text" : "hello" , "meta" : { "skillTokenRanges" : [[0 , 5]] } , } ,] , "_meta" : { "promptId" : "prompt-1" , "screenMode" : "fullscreen" , "sendNow" : true , } , });
        let request = parse_prompt_request(&params, Some(json!(7))).unwrap();
        assert_eq!(request.session_id, "session-1");
        assert_eq!(request.prompt_id.as_deref(), Some("prompt-1"));
        assert_eq!(request.screen_mode.as_deref(), Some("fullscreen"));
        assert!(request.send_now);
        assert_eq!(request.id, Some(json!(7)));
        assert_eq!(request.prompt.len(), 1);
        assert!(request.prompt[0].has_skill_token_ranges());
        assert_eq!(request.prompt[0].bash_command(), None);
    }

    #[test]
    fn parse_prompt_request_defaults_optional_meta() {
        let params = json ! ({ "sessionId" : "session-1" , "prompt" : [text_block ("hi")] , });
        let request = parse_prompt_request(&params, None).unwrap();
        assert_eq!(request.prompt_id, None);
        assert_eq!(request.screen_mode, None);
        assert!(!request.send_now);
        assert!(!request.prompt[0].has_skill_token_ranges());
    }

    #[test]
    fn parse_prompt_request_rejects_missing_session_id() {
        let params = json!({"prompt": [text_block("hi")]});
        assert!(parse_prompt_request(&params, None).is_err());
    }

    #[test]
    fn parse_prompt_request_rejects_unknown_screen_mode() {
        let params = json!({
            "sessionId": "session-1",
            "prompt": [text_block("hi")],
            "_meta": {"screenMode": "sideways"},
        });
        assert!(parse_prompt_request(&params, None).is_err());
    }

    #[test]
    fn parse_prompt_request_rejects_empty_prompt() {
        let params = json!({"sessionId": "session-1", "prompt": []});
        assert!(parse_prompt_request(&params, None).is_err());
    }

    #[test]
    fn parse_prompt_request_rejects_block_without_text() {
        let params = json!({"sessionId": "session-1", "prompt": [{"type": "text"}]});
        assert!(parse_prompt_request(&params, None).is_err());
    }

    #[test]
    fn parse_cancel_notification_reads_audited_meta() {
        let params = json ! ({ "sessionId" : "session-1" , "_meta" : { "cancelTrigger" : "user" , "rewindIfNoOutput" : true , "rewindIfPristine" : true , "promptId" : "prompt-1" , } , });
        let notification = parse_cancel_notification(&params).unwrap();
        assert_eq!(notification.session_id, "session-1");
        assert_eq!(notification.cancel_trigger.as_deref(), Some("user"));
        assert!(notification.rewind_if_no_output);
        assert!(notification.rewind_if_pristine);
        assert_eq!(notification.prompt_id.as_deref(), Some("prompt-1"));
        let meta = notification.meta();
        assert_eq!(meta["cancelTrigger"], json!("user"));
        assert_eq!(meta["rewindIfNoOutput"], json!(true));
        assert_eq!(meta["rewindIfPristine"], json!(true));
        assert_eq!(meta["promptId"], json!("prompt-1"));
    }

    #[test]
    fn parse_cancel_notification_omits_absent_optional_keys() {
        let params = json ! ({ "sessionId" : "session-1" , "_meta" : { } });
        let notification = parse_cancel_notification(&params).unwrap();
        assert_eq!(notification.cancel_trigger, None);
        assert!(!notification.rewind_if_no_output);
        assert!(!notification.rewind_if_pristine);
        assert_eq!(notification.prompt_id, None);
        let meta = notification.meta();
        assert!(meta.get("cancelTrigger").is_none());
        assert!(meta.get("promptId").is_none());
    }

    #[test]
    fn parse_cancel_notification_requires_session_id() {
        assert!(parse_cancel_notification(&json!({"_meta": {}})).is_err());
    }

    #[test]
    fn stop_reason_projection_prefers_interrupted_lifecycle() {
        assert_eq!(
            stop_reason_from_rows("interrupted", None),
            Some(StopReason::Cancelled)
        );
        assert_eq!(
            stop_reason_from_rows("completed", None),
            Some(StopReason::EndTurn)
        );
        assert_eq!(
            stop_reason_from_rows("failed", None),
            Some(StopReason::Error)
        );
        assert_eq!(stop_reason_from_rows("processing", None), None);
    }

    #[test]
    fn stop_reason_projection_maps_interrupted_at_marker() {
        assert_eq!(
            stop_reason_from_rows("processing", Some("2026-01-01T00:00:00Z")),
            None
        );
        assert_eq!(stop_reason_from_rows("processing", Some("  ")), None);
        assert_eq!(
            stop_reason_from_rows("superseded", Some("2026-01-01T00:00:00Z")),
            Some(StopReason::Cancelled)
        );
        assert_eq!(
            stop_reason_from_rows("superseded", None),
            Some(StopReason::Error)
        );
    }

    #[test]
    fn prompt_result_reports_an_errored_turn_as_a_jsonrpc_error() {
        let error = prompt_result(TerminalOutcome {
            stop_reason: StopReason::Error,
            error_message: Some("fail-closed: tool_use observed (bash_unrestricked)".to_string()),
        })
        .unwrap_err();
        assert!(error.to_string().contains("bash_unrestricked"), "{error}");

        let error = prompt_result(StopReason::Error.into()).unwrap_err();
        assert_eq!(error.to_string(), "gents request failed");

        assert_eq!(
            prompt_result(StopReason::EndTurn.into()).unwrap(),
            json!({"stopReason": "end_turn"})
        );
        assert_eq!(
            prompt_result(StopReason::Cancelled.into()).unwrap(),
            json!({"stopReason": "cancelled"})
        );
    }

    #[test]
    fn stop_reason_wire_names_are_audited_values() {
        assert_eq!(StopReason::EndTurn.wire_name(), "end_turn");
        assert_eq!(StopReason::Cancelled.wire_name(), "cancelled");
        assert_eq!(StopReason::Refusal.wire_name(), "refusal");
        assert_eq!(StopReason::Error.wire_name(), "error");
        assert_eq!(stop_reason_from_wire("cancelled"), StopReason::Cancelled);
        assert_eq!(stop_reason_from_wire("end_turn"), StopReason::EndTurn);
        assert_eq!(stop_reason_from_wire("nonsense"), StopReason::Cancelled);
    }

    #[test]
    fn prompt_text_joins_text_blocks_and_preserves_other_kinds() {
        let request = PromptRequest {
            session_id: "s".to_string(),
            prompt: vec![
                PromptBlock {
                    kind: "text".to_string(),
                    text: "one".to_string(),
                    meta: None,
                },
                PromptBlock {
                    kind: "image".to_string(),
                    text: "binary".to_string(),
                    meta: Some(json!({"mime": "image/png"})),
                },
            ],
            prompt_id: None,
            screen_mode: None,
            send_now: false,
            id: None,
        };
        let text = prompt_text(&request);
        assert!(text.starts_with("one\n"));
        assert!(text.contains("\"type\":\"image\""));
    }

    #[tokio::test]
    async fn transient_read_retry_budget_is_consecutive_and_bounded() {
        let transient = anyhow::Error::new(
            reqwest::Client::new()
                .post("http://127.0.0.1:1/api/v0/graphql")
                .send()
                .await
                .expect_err("nothing listens on port 1"),
        )
        .context("reading request terminal state");
        let fatal = anyhow::anyhow!("invalid query");
        // DefraDB's HTTP handler already retried auto-commit read conflicts.
        let mut fresh = 0;
        assert!(!register_transient_read_retry(
            &anyhow::anyhow!("transaction conflict. please retry"),
            &mut fresh
        ));
        let mut consecutive = 0;
        for expected in 1..=MAX_TRANSIENT_GRAPHQL_READ_RETRIES {
            assert!(register_transient_read_retry(&transient, &mut consecutive));
            assert_eq!(consecutive, expected);
        }
        assert!(!register_transient_read_retry(&transient, &mut consecutive));
        consecutive = 0;
        assert!(register_transient_read_retry(&transient, &mut consecutive));
        assert!(!register_transient_read_retry(&fatal, &mut consecutive));
    }

    #[tokio::test]
    async fn user_echo_uses_audited_chunk_shape() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sender = PromptSender::Buffer {
            buffer: buffer.clone(),
        };
        let session_updates = SessionUpdateChannel::new(Arc::new(ProjectionSequencer::new()));
        sender
            .send_user_message_chunk(
                &session_updates,
                "session-1",
                "prompt-1",
                &PromptBlock {
                    kind: "text".to_string(),
                    text: "hello".to_string(),
                    meta: None,
                },
                0,
            )
            .await
            .unwrap();
        let lines = buffer.lock().await;
        assert_eq!(lines.len(), 1);
        let value: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(value["method"], json!("session/update"));
        assert_eq!(value["params"]["sessionId"], json!("session-1"));
        assert_eq!(
            value["params"]["update"]["sessionUpdate"],
            json!("user_message_chunk")
        );
        assert_eq!(value["params"]["update"]["content"]["type"], json!("text"));
        assert_eq!(value["params"]["update"]["content"]["text"], json!("hello"));
        assert_eq!(value["params"]["update"]["_meta"]["promptIndex"], json!(0));
        assert_eq!(
            value["params"]["update"]["_meta"]["hideFromScrollback"],
            json!(false)
        );
        assert!(value["params"]["update"]["content"].get("meta").is_none());
        assert_eq!(value["params"]["_meta"]["promptId"], json!("prompt-1"));
        assert_eq!(value["params"]["_meta"]["isReplay"], json!(false));
        assert_eq!(value["params"]["_meta"]["totalTokens"], json!(0));
        assert_eq!(value["params"]["_meta"]["eventId"], json!("session-1-1"));
    }

    use axum::{extract::State, routing::post, Json, Router};

    #[derive(Clone)]
    struct MockGraphqlState {
        node: Arc<EmbeddedNode>,
        gate_armed: Option<Arc<std::sync::atomic::AtomicBool>>,
        submission_arrived: Option<Arc<tokio::sync::Notify>>,
        submission_release: Option<Arc<tokio::sync::Notify>>,
        submission_fail: Option<Arc<std::sync::atomic::AtomicBool>>,
    }

    async fn mock_graphql(
        State(state): State<MockGraphqlState>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let is_create_request = query.contains("create_AgentRequest");
        tracing::debug!(
            target: "grok_shim::mock",
            query_len = query.len(),
            is_create_request,
            "mock handler forwarding query"
        );
        if is_create_request {
            let first_gated_submission = state
                .gate_armed
                .as_ref()
                .is_some_and(|armed| armed.swap(false, std::sync::atomic::Ordering::SeqCst));
            if first_gated_submission {
                if let Some(arrived) = state.submission_arrived.as_ref() {
                    arrived.notify_one();
                }
                if let Some(release) = state.submission_release.as_ref() {
                    release.notified().await;
                }
                if let Some(fail) = state.submission_fail.as_ref() {
                    if fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        return Json(json!({
                            "errors": [{"message": "injected submission failure"}],
                        }));
                    }
                }
            }
        }
        let response = state.node.execute(&query).await;
        Json(serde_json::to_value(&response).unwrap_or_default())
    }

    async fn spawn_mock_graphql(node: Arc<EmbeddedNode>) -> String {
        spawn_gated_mock_graphql(node, None).await
    }

    async fn spawn_gated_mock_graphql(
        node: Arc<EmbeddedNode>,
        submission_gate: Option<(
            Arc<tokio::sync::Notify>,
            Arc<tokio::sync::Notify>,
            Arc<std::sync::atomic::AtomicBool>,
        )>,
    ) -> String {
        spawn_gated_mock_graphql_with_failure(node, submission_gate, None).await
    }

    async fn spawn_gated_mock_graphql_with_failure(
        node: Arc<EmbeddedNode>,
        submission_gate: Option<(
            Arc<tokio::sync::Notify>,
            Arc<tokio::sync::Notify>,
            Arc<std::sync::atomic::AtomicBool>,
        )>,
        submission_fail: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> String {
        let (submission_arrived, submission_release, gate_armed) = match submission_gate {
            Some((arrived, release, armed)) => (Some(arrived), Some(release), Some(armed)),
            None => (None, None, None),
        };
        let state = MockGraphqlState {
            node,
            gate_armed,
            submission_arrived,
            submission_release,
            submission_fail,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock graphql listener");
        let addr = listener
            .local_addr()
            .expect("mock graphql listener address");
        let router = Router::new()
            .route("/", post(mock_graphql))
            .with_state(state);
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("http://{addr}/")
    }

    async fn test_node() -> (tempfile::TempDir, Arc<EmbeddedNode>, String) {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let identity = gents::KeyIdentity::load_or_create(tempdir.path().join("agent.key"), None)
            .expect("test signing identity");
        let node_did = gents::NodeIdentity::did(&identity).to_string();
        let agent_id = gents::default_agent_id_for_node(&node_did);
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .data_path(tempdir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("embedded node"),
        );
        gents::schema::ensure_runtime_schemas(&node)
            .await
            .expect("runtime schemas");
        super::super::seed_test_agent_configuration(
            &node,
            &node_did,
            &agent_id,
            &agent_id,
            "GLM-5.3-NVFP4",
            true,
        )
        .await;
        (tempdir, node, node_did)
    }

    fn test_config(graphql: String, node_did: &str) -> TurnManagerConfig {
        TurnManagerConfig {
            actor: ::identity::Did::new(node_did.to_string()).expect("fixture creator DID"),
            node_did: node_did.to_string(),
            agent_id: gents::default_agent_id_for_node(node_did),
            graphql,
        }
    }

    fn test_engine(node: Arc<EmbeddedNode>) -> Arc<ProjectionEngine> {
        Arc::new(ProjectionEngine::new(
            node,
            super::super::projection::BoundModelContext::new(
                "GLM-5.3-NVFP4".to_string(),
                "GLM-5.3-NVFP4".to_string(),
                262_144,
            ),
        ))
    }

    async fn seed_runtime_wake(
        node: &EmbeddedNode,
        node_did: &str,
        content: &str,
    ) -> gents_protocol::row::AgentRequestRow {
        let agent_id = gents::default_agent_id_for_node(node_did);
        let request = uuid::Uuid::new_v4().to_string();
        let result = node
            .execute(&format!(
                r#"mutation {{create_AgentRequest(input:{{
            request_id:"{request}",purpose:"normal",node_did:"{node_did}",requester_did:"{node_did}",
            agent_id:"{agent_id}",session_id:"session-1",runtime_source_kind:"local-control",lifecycle_state:"pending",
            content:"{}",created_at:"{}"}}) {{_docID}}}}"#,
                escape_graphql_string(content),
                chrono::Utc::now().to_rfc3339(),
                node_did = escape_graphql_string(node_did)
            ))
            .await;
        ensure_no_errors(&result, "seed runtime wake fixture").unwrap();
        let doc = gents_protocol::graphql::extract_mutation_doc_id(
            &json!({"data":result.data}),
            "AgentRequest",
        )
        .unwrap();
        serde_json::from_value(json!({"_docID":doc,"request_id":request,"node_did":node_did,"requester_did":node_did,"agent_id":agent_id,"session_id":"session-1","runtime_source_kind":"local-control"})).unwrap()
    }

    #[test]
    fn job_tick_runs_before_observe_session_tick() {
        let source = include_str!("turn.rs");
        let observer = &source[source.find("async fn observe_session_since").unwrap()..];
        let before_cards = &observer[..observer.find(".observe_session_tick(").unwrap()];
        assert!(
            before_cards.matches("job_cursor").count() >= 2,
            "the job cursor is declared and refreshed ahead of the card tick"
        );
    }

    fn buffer_sender() -> (Arc<Mutex<Vec<String>>>, PromptSender) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        (buffer.clone(), PromptSender::Buffer { buffer })
    }

    fn fixture_request_filter(request: &gents_protocol::row::AgentRequestRow) -> String {
        format!(
            "{}, _docID: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}",
            gents::session::session_scope_filter(
                request.node_did.as_deref().expect("fixture owner"),
                request.session_id.as_deref().expect("fixture session"),
                request.requester_did.as_deref()
            ),
            escape_graphql_string(request.doc_id.as_deref().expect("fixture physical request")),
            escape_graphql_string(&request.request_id)
        )
    }

    async fn terminalize_request(
        node: &Arc<EmbeddedNode>,
        request: &gents_protocol::row::AgentRequestRow,
        lifecycle_state: &str,
    ) {
        let filter = fixture_request_filter(request);
        let state = escape_graphql_string(lifecycle_state);
        let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let response = node.execute(&format!(r#"mutation {{
            update_AgentRequest(filter: {{{filter}}}, input: {{lifecycle_state: "{state}", terminalized_at: "{now}"}}) {{_docID}}
        }}"#)).await;
        ensure_no_errors(&response, "test terminalize request").expect("terminalize");
    }

    async fn seed_assistant_message(
        node: &Arc<EmbeddedNode>,
        request: &gents_protocol::row::AgentRequestRow,
        _sequence: i64,
        text: &str,
    ) {
        let sequence = i64::from(
            crate::commands::grok_shim::test_fixtures::next_request_sequence(
                node,
                request.doc_id.as_deref().expect("fixture request doc"),
            )
            .await,
        );
        seed_canonical_text_message(
            node,
            request,
            &gents::session::sequence_message_key(
                request.node_did.as_deref().expect("fixture owner"),
                request.session_id.as_deref().expect("fixture session"),
                request.requester_did.as_deref(),
                sequence.try_into().expect("fixture sequence fits u32"),
            ),
            &format!("turn:{sequence}"),
            sequence,
            gents_protocol::output::MessageRole::Assistant,
            text,
        )
        .await;
    }

    async fn seed_completion_notice(
        node: &Arc<EmbeddedNode>,
        request: &gents_protocol::row::AgentRequestRow,
        _sequence: i64,
        text: &str,
    ) {
        let sequence = i64::from(
            crate::commands::grok_shim::test_fixtures::next_request_sequence(
                node,
                request.doc_id.as_deref().expect("fixture request doc"),
            )
            .await,
        );
        seed_canonical_text_message(
            node,
            request,
            &format!("background-completion-notification:{sequence}:tool"),
            &format!("background-completion-notification:{sequence}:tool"),
            sequence,
            gents_protocol::output::MessageRole::User,
            text,
        )
        .await;
    }

    async fn seed_canonical_text_message(
        node: &Arc<EmbeddedNode>,
        request: &gents_protocol::row::AgentRequestRow,
        message_key: &str,
        source_key: &str,
        sequence: i64,
        role: gents_protocol::output::MessageRole,
        text: &str,
    ) {
        use gents::session::canonical_rows::{
            output_segment_create_variables, transcript_message_create_variables,
            CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        };
        use gents_protocol::output::{
            MessageBlock, MessagePublication, OutputOutcome, OutputSegment, OutputSource,
            OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun,
            SourceClose, StreamDeclaration, StreamPayload, TranscriptMessage,
        };

        let node_did = request.node_did.as_deref().expect("fixture owner");
        let session_id = request.session_id.as_deref().expect("fixture session");
        let request_doc_id = request.doc_id.as_deref().expect("fixture physical request");
        let execution_generation = format!("fixture:{request_doc_id}");
        let created_at = chrono::Utc::now().to_rfc3339();
        let segment = OutputSegment {
            node_did: node_did.into(),
            requester_did: request.requester_did.clone(),
            session_id: session_id.into(),
            request_doc_id: request_doc_id.into(),
            source: OutputSource::Authored {
                key: source_key.into(),
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: execution_generation.clone(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: text.len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: text.into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![text.len() as u64],
            }),
            created_at: created_at.clone(),
        };
        let response = node
            .execute_request(
                gents::defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(&segment).unwrap()),
            )
            .await;
        ensure_no_errors(&response, "seed canonical output segment").expect("seed segment");
        let close_doc_id =
            gents::graphql::single_mutation_document(&response, "create_AgentOutputSegment")
                .expect("segment mutation envelope")
                .expect("created segment row")["_docID"]
                .as_str()
                .expect("physical segment id")
                .to_string();
        let message = TranscriptMessage {
            message_key: message_key.into(),
            session_id: session_id.into(),
            node_did: node_did.into(),
            requester_did: request.requester_did.clone(),
            request_doc_id: Some(request_doc_id.into()),
            publication: MessagePublication::RequestExecution {
                execution_generation,
            },
            outcome: OutputOutcome::Complete,
            sequence: sequence.try_into().expect("fixture sequence fits u32"),
            role,
            native_id: None,
            blocks: vec![MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            }],
            created_at,
        };
        let response = node
            .execute_request(
                gents::defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                    .with_variables(transcript_message_create_variables(&message).unwrap()),
            )
            .await;
        ensure_no_errors(&response, "seed canonical transcript header")
            .expect("seed transcript header");
    }

    async fn seed_tool_call(
        node: &Arc<EmbeddedNode>,
        request: &gents_protocol::row::AgentRequestRow,
        tool_call_id: &str,
        tool_name: &str,
        lifecycle_state: &str,
        result: &str,
    ) -> String {
        crate::commands::grok_shim::test_fixtures::seed_canonical_tool_call(
            node,
            request,
            tool_call_id,
            tool_name,
            lifecycle_state,
            "{}",
            matches!(
                lifecycle_state,
                "completed" | "failed" | "cancelled" | "timedOut"
            )
            .then_some(result),
            None,
            None,
            None,
        )
        .await
    }

    async fn seed_child_request(
        node: &Arc<EmbeddedNode>,
        parent: &gents_protocol::row::AgentRequestRow,
        tool_doc_id: &str,
        child_request_id: &str,
        lifecycle_state: &str,
    ) -> gents_protocol::row::AgentRequestRow {
        let owner = escape_graphql_string(parent.node_did.as_deref().unwrap());
        let requester = format!("\"{owner}\"");
        let agent = escape_graphql_string(parent.agent_id.as_deref().expect("fixture agent"));
        let logical_parent = escape_graphql_string(&parent.request_id);
        let physical_parent = escape_graphql_string(parent.doc_id.as_deref().unwrap());
        let tool = escape_graphql_string(tool_doc_id);
        let child = escape_graphql_string(child_request_id);
        let state = escape_graphql_string(lifecycle_state);
        let now = chrono::Utc::now().to_rfc3339();
        crate::commands::grok_shim::test_fixtures::seed_started_session(
            node,
            parent.node_did.as_deref().unwrap(),
            "session-1-child",
            parent.node_did.as_deref(),
            parent.agent_id.as_deref().expect("fixture agent"),
            parent.doc_id.as_deref().unwrap(),
            None,
        )
        .await;
        let response = node.execute(&format!(r#"mutation {{create_AgentRequest(input: {{
            request_id: "{child}", purpose: "normal", node_did: "{owner}", requester_did: {requester}, agent_id: "{agent}", session_id: "session-1-child",
            caused_by_parent_request_id: "{logical_parent}", caused_by_parent_request_doc_id: "{physical_parent}", caused_by_parent_tool_call_id: "call-1", caused_by_parent_tool_call_doc_id: "{tool}", content: "child work", lifecycle_state: "{state}", created_at: "{now}"
        }}) {{_docID}} }}"#)).await;
        ensure_no_errors(&response, "test seed child request").unwrap();
        let doc = gents_protocol::graphql::extract_mutation_doc_id(
            &json!({"data":response.data}),
            "AgentRequest",
        )
        .unwrap();
        serde_json::from_value(json!({"_docID":doc,"request_id":child_request_id,"node_did":parent.node_did,"requester_did":parent.node_did,"agent_id":parent.agent_id,"session_id":"session-1-child", "caused_by_parent_request_id":parent.request_id,"caused_by_parent_request_doc_id":parent.doc_id,"caused_by_parent_tool_call_id":"call-1","caused_by_parent_tool_call_doc_id":tool_doc_id})).unwrap()
    }

    async fn complete_tool_call(node: &Arc<EmbeddedNode>, tool_doc_id: &str, result: &str) {
        let escaped_id = escape_graphql_string(tool_doc_id);
        let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{_docID: {{_eq: "{escaped_id}"}}}}, limit: 2) {{tool_call_id request_doc_id}} }}"#)).await;
        ensure_no_errors(&response, "load tool completion owner").unwrap();
        let rows = response.data.as_ref().unwrap()["AgentToolCall"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1, "tool completion owner must be exact");
        let call_id = rows[0]["tool_call_id"].as_str().unwrap().to_owned();
        let request_doc_id = escape_graphql_string(rows[0]["request_doc_id"].as_str().unwrap());
        let response = node.execute(&format!(r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{request_doc_id}"}}}}, limit: 2) {{_docID request_id node_did requester_did session_id}} }}"#)).await;
        ensure_no_errors(&response, "load tool request owner").unwrap();
        let rows = response.data.as_ref().unwrap()["AgentRequest"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1, "tool request owner must be exact");
        let request = serde_json::from_value(rows[0].clone()).unwrap();
        crate::commands::grok_shim::test_fixtures::complete_canonical_tool_call(
            node,
            &request,
            tool_doc_id,
            &call_id,
            result,
        )
        .await;
    }

    async fn complete_child_request(
        node: &Arc<EmbeddedNode>,
        request: &gents_protocol::row::AgentRequestRow,
    ) {
        let filter = fixture_request_filter(request);
        let escaped_state = escape_graphql_string("completed");
        let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{{filter}}},
                    input: {{
                        lifecycle_state: "{escaped_state}"
                        terminalized_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#
        );
        let response = node.execute(&mutation).await;
        ensure_no_errors(&response, "test complete child request").expect("complete child request");
    }

    #[tokio::test]
    async fn fixture_receipts_keep_colliding_requests_and_child_owners_separate() {
        let (_dir, node, node_did) = test_node().await;
        let agent_id = gents::default_agent_id_for_node(&node_did);
        let mut receipts = Vec::new();
        for content in ["selected", "same-label other document"] {
            let result = node.execute(&format!(r#"mutation {{create_AgentRequest(input: {{
                request_id:"collision", purpose:"normal", node_did:"{node_did}", requester_did:"{node_did}",
                session_id:"session-1", agent_id:"{agent_id}", content:"{content}", lifecycle_state:"pending"
            }}) {{_docID}} }}"#)).await;
            ensure_no_errors(&result, "seed colliding requests").unwrap();
            let doc = gents_protocol::graphql::extract_mutation_doc_id(
                &json!({"data":result.data}),
                "AgentRequest",
            )
            .unwrap();
            receipts.push(serde_json::from_value::<gents_protocol::row::AgentRequestRow>(json!({
                "_docID":doc,"request_id":"collision","node_did":node_did,"requester_did":node_did,
                "session_id":"session-1","agent_id":agent_id
            })).unwrap());
        }
        assert_ne!(receipts[0].doc_id, receipts[1].doc_id);
        let selected = &receipts[0];
        seed_assistant_message(&node, selected, 1, "selected output").await;
        let tool = seed_tool_call(&node, selected, "call-1", "agent_new", "running", "").await;
        let child = seed_child_request(&node, selected, &tool, "child", "processing").await;
        assert_eq!(child.node_did.as_deref(), Some(node_did.as_str()));
        assert_eq!(child.requester_did.as_deref(), Some(node_did.as_str()));
        seed_assistant_message(&node, &child, 1, "child output").await;
        complete_child_request(&node, &child).await;
        complete_tool_call(&node, &tool, "done").await;
        terminalize_request(&node, selected, "completed").await;
        for (index, receipt) in receipts.iter().enumerate() {
            let query = format!(
                "{{AgentRequest(filter: {{{}}}) {{lifecycle_state}} AgentMessage(filter: {{request_doc_id: {{_eq: \"{}\"}}}}) {{message_key node_did requester_did request_doc_id}}}}",
                fixture_request_filter(receipt),
                escape_graphql_string(receipt.doc_id.as_deref().unwrap())
            );
            let result = node.execute(&query).await;
            ensure_no_errors(&result, "read fixture effects").unwrap();
            let data = result.data.unwrap();
            assert_eq!(
                data["AgentRequest"][0]["lifecycle_state"],
                if index == 0 { "completed" } else { "pending" }
            );
            for collection in ["AgentMessage"] {
                let rows = data[collection].as_array().unwrap();
                assert_eq!(
                    rows.len(),
                    if index == 0 { 3 } else { 0 },
                    "the selected request owns its assistant output, canonical tool admission, and exact delivery; the colliding request owns none"
                );
                if index == 0 {
                    assert_eq!(
                        rows.iter()
                            .filter(|row| row["message_key"]
                                .as_str()
                                .is_some_and(|key| key.starts_with("accepted:")))
                            .count(),
                        1,
                        "exactly one coordinator admission header is owned by the selected request"
                    );
                    assert_eq!(
                        rows.iter()
                            .filter(|row| row["message_key"]
                                .as_str()
                                .is_some_and(|key| key.starts_with("delivery:")))
                            .count(),
                        1,
                        "exactly one physical ToolDelivery header is owned by the selected request"
                    );
                }
                for row in rows {
                    assert_eq!(row["node_did"], node_did);
                    assert_eq!(row["requester_did"], node_did);
                    assert_eq!(row["request_doc_id"], receipt.doc_id.as_deref().unwrap());
                }
            }
        }
    }

    async fn parse_buffered_updates(buffer: &Arc<Mutex<Vec<String>>>) -> Vec<Value> {
        let lines = buffer.lock().await.clone();
        lines
            .iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect()
    }

    async fn buffered_update_kinds(buffer: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        parse_buffered_updates(buffer)
            .await
            .iter()
            .map(|value| {
                value["params"]["update"]["sessionUpdate"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    #[tokio::test]
    async fn prompt_resolves_end_turn_after_terminalization() {
        let (_tempdir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
        let engine = test_engine(node.clone());
        let (_buffer, sender) = buffer_sender();

        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();

        let node_for_terminalize = node.clone();
        let did_for_terminalize = node_did.clone();
        let handle = tokio::spawn(async move {
            loop {
                let scope = gents::session::session_scope_filter(
                    &did_for_terminalize,
                    "session-1",
                    Some(&did_for_terminalize),
                );
                let query = format!(
                    "{{AgentRequest(filter: {{{scope}, lifecycle_state: {{_eq: \"pending\"}}}}) {{{}}}}}",
                    gents::SIGNED_REQUEST_FIELDS
                );
                let response = node_for_terminalize.execute(&query).await;
                ensure_no_errors(&response, "pending fixture rows").unwrap();
                let rows = response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .and_then(Value::as_array)
                    .cloned()
                    .expect("pending fixture rows");
                assert!(rows.len() <= 1, "ambiguous pending fixture request");
                if let Some(row) = rows.first() {
                    let receipt: gents_protocol::row::AgentRequestRow =
                        serde_json::from_value(row.clone()).unwrap();
                    terminalize_request(&node_for_terminalize, &receipt, "completed").await;
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });

        let result = tokio::time::timeout(
            Duration::from_secs(30),
            manager.handle_prompt(prompt, &sender, &engine),
        )
        .await
        .expect("prompt should resolve within timeout")
        .expect("prompt should succeed");
        assert_eq!(result["stopReason"], json!("end_turn"));
        handle.await.expect("terminalize task");
    }

    #[tokio::test]
    async fn goal_prompt_atomically_submits_scoped_goal_and_signed_request() {
        let (_dir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
        let mut prompt = parse_prompt_request(
            &json!({
                "sessionId":"goal-submit-session",
                "prompt":[text_block("/goal Explain the architecture --budget 100000")],
                "_meta":{"screenMode":"inline", "sendNow":true}
            }),
            None,
        )
        .unwrap();
        let id_receipt = manager
            .submit_request(&prompt, "goal-prompt-id")
            .await
            .unwrap();
        let id = id_receipt.request_id.clone();
        let goal = gents::goal::load_canonical_goal(&node, &node_did, &prompt.session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(goal.objective, "Explain the architecture");
        assert_eq!(goal.status, "active");
        assert_eq!(goal.token_budget, Some(100000));
        let response = node.execute(&format!(
            "{{AgentRequest(filter:{{request_id:{{_eq:\"{id}\"}}}}){{request_id node_did session_id content input retry_key admission_signer_did}}}}"
        )).await;
        gents::graphql::ensure_no_errors(&response, "goal submission").unwrap();
        let row = &response.data.as_ref().unwrap()["AgentRequest"][0];
        assert_eq!(row["content"], "Explain the architecture");
        assert_eq!(row["node_did"], node_did);
        assert_eq!(row["admission_signer_did"], node_did);
        assert_eq!(row["session_id"], prompt.session_id);
        assert_eq!(row["retry_key"], format!("goal-request:{id}"));
        assert!(row["input"].is_null());

        prompt.prompt[0].text = "/goal Conflicting objective".into();
        assert!(manager.submit_request(&prompt, "conflict").await.is_err());
        let response = node
            .execute("{AgentRequest{request_id} Goal{goal_id} GoalCreationClaim{creation_key}}")
            .await;
        gents::graphql::ensure_no_errors(&response, "atomic rejection").unwrap();
        let data = response.data.as_ref().unwrap();
        for collection in ["AgentRequest", "Goal", "GoalCreationClaim"] {
            assert_eq!(
                data[collection].as_array().unwrap().len(),
                1,
                "{collection}"
            );
        }
        assert_eq!(
            gents::goal::load_canonical_goal(&node, &node_did, &prompt.session_id)
                .await
                .unwrap()
                .unwrap()
                .objective,
            "Explain the architecture"
        );
        gents::goal::delete_goals_for_session(&node, &node_did, &prompt.session_id)
            .await
            .unwrap();
        let next_receipt = manager
            .submit_request(&prompt, "new-incarnation")
            .await
            .unwrap();
        let next = next_receipt.request_id.clone();
        assert_ne!(id, next);
    }

    #[tokio::test]
    async fn resume_hands_off_active_and_during_replay_requests_without_repeating_history() {
        let (_dir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
        let engine = test_engine(node.clone());
        let (buffer, sender) = buffer_sender();
        let prompt = parse_prompt_request(
            &json!({"sessionId":"session-1", "prompt":[text_block("Original prompt")]}),
            None,
        )
        .unwrap();
        let first_receipt = manager.submit_request(&prompt, "first").await.unwrap();
        let first = first_receipt.request_id.clone();
        seed_assistant_message(&node, &first_receipt, 1, "Before reconnect.").await;
        let attached = chrono::Utc::now().to_rfc3339();
        let rows = super::super::sessions::requests(&node, &node_did, Some("session-1"))
            .await
            .unwrap();
        let running = manager
            .replay_session("session-1", &rows, &sender, &engine)
            .await
            .unwrap();
        assert_eq!(running, Some(format!("notifications-{first}")));
        let historical = parse_buffered_updates(&buffer).await;
        assert!(historical
            .iter()
            .all(|event| event["params"]["_meta"]["isReplay"] == true));
        buffer.lock().await.clear();
        seed_assistant_message(&node, &first_receipt, 2, "After reconnect.").await;
        terminalize_request(&node, &first_receipt, "completed").await;
        let second_receipt = manager.submit_request(&prompt, "second").await.unwrap();
        seed_assistant_message(&node, &second_receipt, 3, "Created during replay.").await;
        terminalize_request(&node, &second_receipt, "completed").await;
        let mut after = String::new();
        let mut delivery_after = String::new();
        for _ in 0..3 {
            manager
                .observe_session_tick(
                    "session-1",
                    &attached,
                    &mut after,
                    &mut delivery_after,
                    &sender,
                    &engine,
                )
                .await
                .unwrap();
        }
        let updates = parse_buffered_updates(&buffer).await;
        let text = updates
            .iter()
            .filter_map(|event| {
                event
                    .pointer("/params/update/content/text")
                    .and_then(Value::as_str)
            })
            .collect::<String>();
        assert!(!text.contains("Before reconnect."));
        assert_eq!(text.matches("After reconnect.").count(), 1);
        assert_eq!(text.matches("Created during replay.").count(), 1);
        assert!(updates
            .iter()
            .all(|event| event["params"]["_meta"]["isReplay"] != true));
        assert!(manager.autonomous_delivery.lock().await.is_empty());
    }

    #[tokio::test]
    async fn session_discovery_excludes_foreign_nodes_and_requesters() {
        let (_tempdir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
        let engine = test_engine(node.clone());
        let (_, sender) = buffer_sender();
        for (id, owner, requester) in [
            ("foreign-agent", "did:test:foreign", Some(node_did.as_str())),
            (
                "foreign-requester",
                node_did.as_str(),
                Some("did:test:foreign"),
            ),
            ("missing-requester", node_did.as_str(), None),
            ("owned", node_did.as_str(), Some(node_did.as_str())),
        ] {
            let requester = requester
                .map(|did| format!("\"{}\"", escape_graphql_string(did)))
                .unwrap_or_else(|| "null".into());
            let result = node
                .execute(&format!(
                    r#"mutation {{ create_AgentRequest(input: {{
                request_id: "{id}", purpose: "normal", session_id: "session-1", node_did: "{}",
                requester_did: {requester}, lifecycle_state: "completed",
                created_at: "2026-06-04T12:00:00Z"
            }}) {{ _docID }} }}"#,
                    escape_graphql_string(owner)
                ))
                .await;
            ensure_no_errors(&result, "seed scoped session discovery").unwrap();
        }
        manager
            .observe_session_tick(
                "session-1",
                "1970-01-01T00:00:00Z",
                &mut String::new(),
                &mut String::new(),
                &sender,
                &engine,
            )
            .await
            .unwrap();
        let observed = manager.observed.lock().await;
        assert_eq!(observed.len(), 1);
        assert!(observed.contains_key(&("session-1".into(), "owned".into())));
    }

    #[tokio::test]
    async fn observed_human_turn_keeps_prompt_identity_echo_and_cancel_target() {
        let (_tempdir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
        let engine = test_engine(node.clone());
        let (buffer, sender) = buffer_sender();
        let prompt = parse_prompt_request(
            &json!({"sessionId":"session-1", "prompt":[text_block("Human from another client")]}),
            None,
        )
        .unwrap();
        let request_receipt = manager.submit_request(&prompt, "peer-human").await.unwrap();
        let request = request_receipt.request_id.clone();
        seed_assistant_message(&node, &request_receipt, 1, "Peer answer").await;
        let mut after = String::new();
        let mut delivery_after = String::new();
        manager
            .observe_session_tick(
                "session-1",
                "1970-01-01T00:00:00Z",
                &mut after,
                &mut delivery_after,
                &sender,
                &engine,
            )
            .await
            .unwrap();
        let events: Vec<Value> = buffer
            .lock()
            .await
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let echo = events
            .iter()
            .find(|event| event["params"]["update"]["sessionUpdate"] == "user_message_chunk")
            .unwrap();
        assert_eq!(echo["params"]["_meta"]["promptId"], request);
        assert_eq!(
            echo["params"]["update"]["_meta"]["hideFromScrollback"],
            false
        );
        assert_eq!(
            echo["params"]["update"]["content"]["text"],
            "Human from another client"
        );
        for target in ["different-prompt", request.as_str()] {
            manager
                .handle_cancel(
                    parse_cancel_notification(
                        &json!({"sessionId":"session-1", "_meta":{"promptId":target}}),
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            let interrupted = node.execute(&format!(r#"{{AgentRequest(filter:{{request_id:{{_eq:"{}"}}}}){{interrupt_requested_at}}}}"#, escape_graphql_string(&request))).await;
            ensure_no_errors(&interrupted, "read viewer cancel intent").unwrap();
            assert_eq!(
                interrupted.data.as_ref().unwrap()["AgentRequest"][0]["interrupt_requested_at"]
                    .as_str()
                    .is_some(),
                target == request
            );
        }
        terminalize_request(&node, &request_receipt, "interrupted").await;
        manager
            .observe_session_tick(
                "session-1",
                "1970-01-01T00:00:00Z",
                &mut after,
                &mut delivery_after,
                &sender,
                &engine,
            )
            .await
            .unwrap();
        let events: Vec<Value> = buffer
            .lock()
            .await
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["params"]["update"]["sessionUpdate"] == "user_message_chunk")
                .count(),
            1
        );
        let completed = events
            .iter()
            .find(|event| event["params"]["update"]["sessionUpdate"] == "turn_completed")
            .unwrap();
        assert_eq!(completed["params"]["update"]["prompt_id"], request);
        assert_eq!(completed["params"]["update"]["stop_reason"], "cancelled");
    }

    #[tokio::test]
    async fn autonomous_turn_defers_late_notices_and_plain_escape_cancels_its_owner() {
        let (_tempdir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
        let engine = test_engine(node.clone());
        let (buffer, sender) = buffer_sender();
        let prompt = parse_prompt_request(
            &json!({"sessionId": "session-1",
            "prompt": [text_block("internal wake instruction")]}),
            None,
        )
        .unwrap();
        let root_receipt = manager.submit_request(&prompt, "root").await.unwrap();
        let root = root_receipt.request_id.clone();
        terminalize_request(&node, &root_receipt, "completed").await;
        manager.observed.lock().await.insert(
            ("session-1".into(), root.clone()),
            Arc::new(Mutex::new(ObservedRequest::new("root".into(), 0, true))),
        );
        let first_receipt = seed_runtime_wake(&node, &node_did, "internal wake instruction").await;
        let first = first_receipt.request_id.clone();
        seed_assistant_message(&node, &first_receipt, 1, "Wake A is working.").await;
        seed_tool_call(&node, &first_receipt, "wake-a-tool", "bash", "running", "").await;
        let mut after = String::new();
        let mut delivery_after = String::new();
        manager
            .observe_session_tick(
                "session-1",
                "1970-01-01T00:00:00Z",
                &mut after,
                &mut delivery_after,
                &sender,
                &engine,
            )
            .await
            .unwrap();
        assert_eq!(
            manager.autonomous_delivery.lock().await.get("session-1"),
            Some(&first)
        );

        for (request, sequence, text) in [
            (&root_receipt, 2, "Root task finished."),
            (&first_receipt, 3, "Wake task finished."),
        ] {
            seed_completion_notice(&node, request, sequence, text).await;
        }
        let second_receipt = seed_runtime_wake(&node, &node_did, "internal wake instruction").await;
        seed_assistant_message(&node, &second_receipt, 4, "Wake B response.").await;
        terminalize_request(&node, &second_receipt, "completed").await;
        seed_assistant_message(&node, &first_receipt, 5, "Wake A continues.").await;
        manager
            .observe_session_tick(
                "session-1",
                "1970-01-01T00:00:00Z",
                &mut after,
                &mut delivery_after,
                &sender,
                &engine,
            )
            .await
            .unwrap();
        let updates = parse_buffered_updates(&buffer).await;
        let text: Vec<_> = updates
            .iter()
            .filter_map(|event| {
                event
                    .pointer("/params/update/content/text")
                    .and_then(Value::as_str)
            })
            .collect();
        assert!(text.contains(&"Wake A continues."));
        assert!(!text.contains(&"Root task finished."));
        assert!(!text.contains(&"Wake task finished."));
        assert!(!text.contains(&"Wake B response."));

        manager
            .handle_cancel(parse_cancel_notification(&json!({"sessionId": "session-1"})).unwrap())
            .await
            .unwrap();
        let interrupted = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }})
            {{ interrupt_requested_at }} }}"#,
                escape_graphql_string(&first)
            ))
            .await;
        ensure_no_errors(&interrupted, "read autonomous interrupt").unwrap();
        assert!(interrupted
            .data
            .as_ref()
            .and_then(|data| data.pointer("/AgentRequest/0/interrupt_requested_at"))
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()));
        terminalize_request(&node, &first_receipt, "interrupted").await;
        for _ in 0..3 {
            manager
                .observe_session_tick(
                    "session-1",
                    "1970-01-01T00:00:00Z",
                    &mut after,
                    &mut delivery_after,
                    &sender,
                    &engine,
                )
                .await
                .unwrap();
        }
        let updates = parse_buffered_updates(&buffer).await;
        let first_complete = updates
            .iter()
            .position(|event| {
                event
                    .pointer("/params/update/prompt_id")
                    .and_then(Value::as_str)
                    == Some(format!("notifications-{first}").as_str())
            })
            .unwrap();
        let second_start = updates
            .iter()
            .position(|event| {
                event
                    .pointer("/params/update/content/text")
                    .and_then(Value::as_str)
                    == Some("Wake B response.")
            })
            .unwrap();
        for notice in ["Root task finished.", "Wake task finished."] {
            let positions: Vec<_> = updates
                .iter()
                .enumerate()
                .filter_map(|(index, event)| {
                    (event
                        .pointer("/params/update/content/text")
                        .and_then(Value::as_str)
                        == Some(notice))
                    .then_some(index)
                })
                .collect();
            assert_eq!(
                positions.len(),
                1,
                "each deferred notice survives the message high-water and is delivered once"
            );
            assert!(first_complete < positions[0] && positions[0] < second_start);
        }
        assert!(manager.autonomous_delivery.lock().await.is_empty());
    }

    #[tokio::test]
    async fn opening_request_is_the_agent_new_call_not_a_same_second_message() {
        let (_tempdir, node, node_did) = test_node().await;
        let agent_id = gents::default_agent_id_for_node(&node_did);
        let node_literal = escape_graphql_string(&node_did);
        let agent_literal = escape_graphql_string(&agent_id);
        let response = node.execute(&format!(r#"mutation {{create_AgentRequest(input: {{
            request_id:"open-root", purpose:"normal", session_id:"session-1", node_did:"{node_literal}", requester_did:"{node_literal}", agent_id:"{agent_literal}", lifecycle_state:"processing"
        }}) {{_docID}} }}"#)).await;
        ensure_no_errors(&response, "seed opening parent").unwrap();
        let parent_doc = gents_protocol::graphql::extract_mutation_doc_id(
            &json!({"data":response.data}),
            "AgentRequest",
        )
        .unwrap();
        let parent: gents_protocol::row::AgentRequestRow = serde_json::from_value(json!({"_docID":parent_doc,"request_id":"open-root","node_did":node_did,"requester_did":node_did,"agent_id":agent_id,"session_id":"session-1"})).unwrap();
        let open_call =
            seed_tool_call(&node, &parent, "call-open", "agent_new", "running", "").await;
        let message_call = seed_tool_call(
            &node,
            &parent,
            "call-message",
            "agent_message",
            "running",
            "",
        )
        .await;
        let mut opening_doc = None;
        for (request_id, call_id, call_doc) in [
            ("zz-opening", "call-open", &open_call),
            ("aa-message", "call-message", &message_call),
        ] {
            let request_id = escape_graphql_string(request_id);
            let parent = escape_graphql_string(&parent_doc);
            let call_id = escape_graphql_string(call_id);
            let call_doc = escape_graphql_string(call_doc);
            let response = node.execute(&format!(r#"mutation {{create_AgentRequest(input: {{
                request_id:"{request_id}", purpose:"normal", session_id:"opened", node_did:"{node_literal}", requester_did:"{node_literal}", agent_id:"{agent_literal}", lifecycle_state:"processing", created_at:"2026-09-27T00:00:00Z",
                caused_by_parent_request_id:"open-root", caused_by_parent_request_doc_id:"{parent}", caused_by_parent_tool_call_id:"{call_id}", caused_by_parent_tool_call_doc_id:"{call_doc}"
            }}) {{_docID}} }}"#)).await;
            ensure_no_errors(&response, "seed opened session request").unwrap();
            if request_id == "zz-opening" {
                opening_doc = Some(
                    gents_protocol::graphql::extract_mutation_doc_id(
                        &json!({"data":response.data}),
                        "AgentRequest",
                    )
                    .unwrap(),
                );
            }
        }
        // The session records its opener: `(created_at, _docID)` alone would
        // tie these two requests on an unfixed document id.
        crate::commands::grok_shim::test_fixtures::seed_started_session(
            &node,
            &node_did,
            "opened",
            Some(&node_did),
            &agent_id,
            &parent_doc,
            opening_doc.as_deref(),
        )
        .await;
        let started = crate::caused_sessions::load_direct_caused_sessions(&node, &parent)
            .await
            .unwrap();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].scope.session_id, "opened");
        assert_eq!(started[0].first.request_id, "zz-opening");
        assert_eq!(
            started[0]
                .first
                .caused_by_parent_tool_call_doc_id
                .as_deref(),
            Some(open_call.as_str())
        );
    }

    #[tokio::test]
    async fn child_pane_streams_followups_before_finish_and_excludes_foreign_requester() {
        let (_tempdir, node, node_did) = test_node().await;
        let manager = TurnManager::new(node.clone(), test_config(String::new(), &node_did));
        let engine = test_engine(node.clone());
        let (buffer, sender) = buffer_sender();
        let agent_id = gents::default_agent_id_for_node(&node_did);
        let response = node.execute(&format!(r#"mutation {{create_AgentRequest(input: {{
            request_id:"pane-root", purpose:"normal", session_id:"session-1", node_did:"{node_did}", requester_did:"{node_did}", agent_id:"{agent_id}", lifecycle_state:"processing"
        }}) {{_docID}} }}"#)).await;
        ensure_no_errors(&response, "seed pane root").unwrap();
        let doc = gents_protocol::graphql::extract_mutation_doc_id(
            &json!({"data":response.data}),
            "AgentRequest",
        )
        .unwrap();
        let parent: gents_protocol::row::AgentRequestRow = serde_json::from_value(json!({"_docID":doc,"request_id":"pane-root","node_did":node_did,"requester_did":node_did,"agent_id":agent_id,"session_id":"session-1"})).unwrap();
        let tool = seed_tool_call(&node, &parent, "call-1", "agent_new", "running", "").await;
        let child = seed_child_request(&node, &parent, &tool, "pane-child", "processing").await;
        let mut followup = None;
        for (id, requester, text) in [
            (
                "pane-child",
                Some(node_did.as_str()),
                "Original child output",
            ),
            (
                "pane-followup",
                Some(node_did.as_str()),
                "Steered child output",
            ),
            ("pane-foreign", Some("did:foreign"), "MUST NOT LEAK"),
        ] {
            let row = if id == "pane-child" {
                child.clone()
            } else {
                let requester_field = requester
                    .map(|did| format!("\"{}\"", escape_graphql_string(did)))
                    .unwrap_or_else(|| "null".into());
                let created_at = chrono::Utc::now().to_rfc3339();
                let response = node.execute(&format!(
                    r#"mutation {{create_AgentRequest(input: {{request_id:"{}", purpose:"normal", session_id:"session-1-child", node_did:"{}", requester_did:{requester_field}, agent_id:"{}", lifecycle_state:"processing", created_at:"{}"}}) {{_docID}} }}"#,
                    escape_graphql_string(id),
                    escape_graphql_string(&node_did),
                    escape_graphql_string(&agent_id),
                    escape_graphql_string(&created_at),
                )).await;
                ensure_no_errors(&response, "seed child followup").unwrap();
                let doc = gents_protocol::graphql::extract_mutation_doc_id(
                    &json!({"data":response.data}),
                    "AgentRequest",
                )
                .unwrap();
                serde_json::from_value(json!({"_docID":doc,"request_id":id,"node_did":node_did,"requester_did":requester,"agent_id":agent_id,"session_id":"session-1-child"})).unwrap()
            };
            let sequence = if id == "pane-followup" { 2 } else { 1 };
            seed_assistant_message(&node, &row, sequence, text).await;
            if id == "pane-followup" {
                followup = Some(row);
            }
        }
        let spawn_doc = crate::commands::grok_shim::test_fixtures::seed_canonical_tool_call(
            &node,
            &child,
            "child-process-meta",
            "spawn_process",
            "completed",
            r#"{"tool_name":"bash","args":{"command":"echo CHILD_BG_OUTPUT"}}"#,
            Some("spawned child-bash"),
            None,
            None,
            None,
        )
        .await;
        let child_tool_doc = crate::commands::grok_shim::test_fixtures::seed_canonical_tool_call(
            &node,
            &child,
            "child-bash",
            "bash",
            "running",
            r#"{"command":"echo CHILD_BG_OUTPUT"}"#,
            None,
            Some(&spawn_doc),
            None,
            None,
        )
        .await;
        let response = node
            .execute(&format!(
                r#"mutation {{ update_AgentToolCall(filter: {{_docID: {{_eq: "{}"}}}}, input: {{await_mode: "background"}}) {{_docID}} }}"#,
                escape_graphql_string(&child_tool_doc)
            ))
            .await;
        ensure_no_errors(&response, "mark child tool backgrounded").unwrap();
        crate::commands::grok_shim::test_fixtures::seed_canonical_live_tool_output(
            node.as_ref(),
            &child,
            &child_tool_doc,
            "CHILD_BG_OUTPUT",
        )
        .await;
        seed_assistant_message(&node, &parent, 7, "PARENT MUST WAIT").await;
        let cursor = Mutex::new(RequestCursor::new());
        cursor.lock().await.request = Some(parent);
        let mut timing = RequestUpdateTiming::new(0);
        manager
            .stream_projection_updates_mode(
                "session-1",
                "pane-root",
                Some("parent-prompt"),
                &sender,
                &engine,
                &cursor,
                &mut timing,
                false,
                0,
                true,
            )
            .await
            .unwrap();
        let activity = parse_buffered_updates(&buffer).await;
        let task_start = activity
            .iter()
            .position(|row| {
                row["params"]["update"]["sessionUpdate"] == "task_backgrounded"
                    && row["params"]["sessionId"] == "session-1-child"
            })
            .unwrap();
        let output = activity
            .iter()
            .position(|row| {
                row["params"]["update"]["sessionUpdate"] == "monitor_event"
                    && row["params"]["sessionId"] == "session-1-child"
            })
            .unwrap();
        assert!(task_start < output);
        assert_eq!(
            activity[output]["params"]["update"]["event_text"],
            "CHILD_BG_OUTPUT"
        );
        assert!(activity
            .iter()
            .any(|row| row["params"]["sessionId"] == "session-1-child"
                && row["params"]["update"]["toolCallId"] == "child-bash"));
        assert!(!activity.iter().any(|row| {
            row.pointer("/params/update/content/text")
                .and_then(Value::as_str)
                == Some("PARENT MUST WAIT")
        }));
        for row in activity
            .iter()
            .filter(|row| row["params"]["sessionId"] == "session-1")
        {
            assert!(row.pointer("/params/_meta/promptId").is_none());
            assert!(row.pointer("/params/_meta/turnStartMs").is_none());
        }
        complete_child_request(&node, &child).await;
        crate::commands::grok_shim::test_fixtures::complete_canonical_spawned_process_output(
            node.as_ref(),
            &child,
            &child_tool_doc,
            "CHILD_BG_OUTPUT",
        )
        .await;
        complete_tool_call(&node, &tool, "done").await;
        manager
            .stream_projection_updates(
                "session-1",
                "pane-root",
                None,
                &sender,
                &engine,
                &cursor,
                &mut timing,
                true,
                0,
            )
            .await
            .unwrap();
        assert!(!buffered_update_kinds(&buffer)
            .await
            .iter()
            .any(|kind| kind == "subagent_finished"));
        complete_child_request(&node, followup.as_ref().unwrap()).await;
        manager
            .stream_projection_updates(
                "session-1",
                "pane-root",
                None,
                &sender,
                &engine,
                &cursor,
                &mut timing,
                true,
                0,
            )
            .await
            .unwrap();
        let updates = parse_buffered_updates(&buffer).await;
        let position = |kind: &str| {
            updates
                .iter()
                .position(|row| row["params"]["update"]["sessionUpdate"] == kind)
                .unwrap()
        };
        let spawned = position("subagent_spawned");
        let finished = position("subagent_finished");
        let child_tools: Vec<_> = updates
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row["params"]["sessionId"] == "session-1-child"
                    && row["params"]["update"]["toolCallId"] == "child-bash"
            })
            .collect();
        assert!(
            !child_tools.is_empty(),
            "child pane receives native tool frames"
        );
        assert!(child_tools
            .iter()
            .all(|(index, _)| spawned < *index && *index < finished));
        for text in ["Original child output", "Steered child output"] {
            let matches: Vec<_> = updates
                .iter()
                .enumerate()
                .filter(|(_, row)| row["params"]["update"]["content"]["text"] == text)
                .collect();
            assert_eq!(matches.len(), 1, "child cursor must not replay {text}");
            assert!(spawned < matches[0].0 && matches[0].0 < finished);
            assert_eq!(matches[0].1["params"]["sessionId"], "session-1-child");
        }
        assert!(!updates
            .iter()
            .any(|row| row.to_string().contains("MUST NOT LEAK")));
    }

    #[tokio::test]
    async fn a_projected_write_wakes_observation_before_the_fallback() {
        let (_tempdir, node, node_did) = test_node().await;
        let mut changes = subscribe_projected_changes(&node);
        assert!(changes.is_some(), "every projected collection is installed");
        let started = tokio::time::Instant::now();
        seed_runtime_wake(&node, &node_did, "wake").await;
        wait_for_projected_change(&mut changes).await;
        assert!(changes.is_some());
        assert!(started.elapsed() < PROJECTION_FALLBACK);
    }

    #[tokio::test]
    async fn session_observer_preserves_handoff_and_delivers_late_wake() {
        let (_tempdir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let engine = test_engine(node.clone());
        let (buffer, sender) = buffer_sender();
        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1", "prompt": [text_block("start")],
                "_meta": {"promptId": "foreground-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();
        let turn = {
            let manager = manager.clone();
            let engine = engine.clone();
            let sender = sender.clone();
            tokio::spawn(async move { manager.handle_prompt(prompt, &sender, &engine).await })
        };
        let root_receipt = wait_for_pending_request(&node, &node_did).await;
        let late_tool_doc =
            seed_tool_call(&node, &root_receipt, "late-bash", "bash", "running", "").await;
        seed_assistant_message(&node, &root_receipt, 1, "Root response.").await;
        let mut after = String::new();
        let mut delivery_after = String::new();
        manager
            .observe_session_tick(
                "session-1",
                "1970-01-01T00:00:00Z",
                &mut after,
                &mut delivery_after,
                &sender,
                &engine,
            )
            .await
            .unwrap();
        terminalize_request(&node, &root_receipt, "completed").await;
        let result = tokio::time::timeout(Duration::from_secs(30), turn)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result["stopReason"], "end_turn");

        let update = node
            .execute(&format!(
                r#"mutation {{ update_AgentToolCall(
            filter: {{ _docID: {{ _eq: "{}" }} }},
            input: {{ lifecycle_state: "failed" }}) {{ _docID }} }}"#,
                escape_graphql_string(&late_tool_doc)
            ))
            .await;
        ensure_no_errors(&update, "late tool finish").unwrap();
        let wake_receipt =
            seed_runtime_wake(&node, &node_did, "internal notification instruction").await;
        let wake = wake_receipt.request_id.clone();
        seed_assistant_message(&node, &wake_receipt, 2, "Background work finished.").await;
        terminalize_request(&node, &wake_receipt, "completed").await;
        let (pending_tx, _pending_rx) = oneshot::channel();
        let foreground_key = ("session-1".to_owned(), "new-foreground".to_owned());
        manager.state.lock().await.entries.insert(
            foreground_key.clone(),
            PendingPrompt {
                response_tx: Some(pending_tx),
                request: Some(
                    serde_json::from_value(json!({"request_id": "new-foreground-request"}))
                        .unwrap(),
                ),
                cancel_before_id: Arc::new(Mutex::new(CancelBeforeIdLatch::default())),
                drained: false,
            },
        );
        manager
            .observe_session_tick(
                "session-1",
                "1970-01-01T00:00:00Z",
                &mut after,
                &mut delivery_after,
                &sender,
                &engine,
            )
            .await
            .unwrap();
        assert!(
            !parse_buffered_updates(&buffer)
                .await
                .iter()
                .any(|event| event
                    .pointer("/params/update/content/text")
                    .and_then(Value::as_str)
                    == Some("Background work finished.")),
            "wake output must remain undelivered while the pager has a foreground prompt"
        );
        assert!(
            parse_buffered_updates(&buffer).await.iter().any(|event| {
                event
                    .pointer("/params/update/toolCallId")
                    .and_then(Value::as_str)
                    == Some("late-bash")
                    && event
                        .pointer("/params/update/status")
                        .and_then(Value::as_str)
                        == Some("failed")
            }),
            "background terminal updates must flow while a new foreground is pending"
        );
        manager.state.lock().await.entries.remove(&foreground_key);
        let response_delivery = manager.begin_foreground_delivery("session-1").await;
        manager
            .observe_session_tick(
                "session-1",
                "1970-01-01T00:00:00Z",
                &mut after,
                &mut delivery_after,
                &sender,
                &engine,
            )
            .await
            .unwrap();
        assert!(!parse_buffered_updates(&buffer).await.iter().any(|event| {
            event
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
                == Some("Background work finished.")
        }));
        drop(response_delivery);
        for _ in 0..2 {
            manager
                .observe_session_tick(
                    "session-1",
                    "1970-01-01T00:00:00Z",
                    &mut after,
                    &mut delivery_after,
                    &sender,
                    &engine,
                )
                .await
                .unwrap();
        }
        let updates = parse_buffered_updates(&buffer).await;
        let text: Vec<_> = updates
            .iter()
            .filter_map(|event| {
                event
                    .pointer("/params/update/content/text")
                    .and_then(Value::as_str)
            })
            .collect();
        assert_eq!(
            text.iter()
                .filter(|value| **value == "Root response.")
                .count(),
            1
        );
        assert_eq!(
            text.iter()
                .filter(|value| **value == "Background work finished.")
                .count(),
            1
        );
        assert!(!text.contains(&"internal notification instruction"));
        let failed = updates
            .iter()
            .find(|event| {
                event
                    .pointer("/params/update/status")
                    .and_then(Value::as_str)
                    == Some("failed")
            })
            .unwrap();
        assert!(
            failed.pointer("/params/_meta/promptId").is_none(),
            "late root update must pass the pager's idle prompt gate"
        );
        let completions: Vec<_> = updates
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                event
                    .pointer("/params/update/sessionUpdate")
                    .and_then(Value::as_str)
                    == Some("turn_completed")
            })
            .collect();
        assert_eq!(completions.len(), 1);
        let (completion_index, completion) = completions[0];
        assert_eq!(completion["method"], "_x.ai/session_notification");
        assert_eq!(
            completion["params"]["update"]["prompt_id"],
            format!("notifications-{wake}")
        );
        assert_eq!(
            completion["params"]["_meta"]["promptId"],
            format!("notifications-{wake}")
        );
        assert!(updates[..completion_index].iter().any(|event| {
            event
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
                == Some("Background work finished.")
        }));

        manager
            .observe_session("session-1", sender.clone(), engine.clone())
            .await;
        let delivery = manager.delivery_gate.lock().await;
        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1", "prompt": [text_block("after wake")],
            }),
            None,
        )
        .unwrap();
        let mut admission = Box::pin(manager.handle_prompt(prompt, &sender, &engine));
        tokio::select! {
            biased;
            result = &mut admission => panic!("admission crossed delivery gate: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
        assert!(manager.state.lock().await.entries.is_empty());
        tokio::time::timeout(Duration::from_secs(5), manager.handle_disconnect())
            .await
            .unwrap()
            .unwrap();
        assert!(manager.observers.lock().await.is_empty());
        drop(delivery);
        assert!(admission
            .await
            .unwrap_err()
            .to_string()
            .contains("disconnected"));
    }

    #[tokio::test]
    async fn live_turn_streams_tool_agent_and_message_updates_once_each() {
        let (_tempdir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let engine = test_engine(node.clone());
        let (buffer, sender) = buffer_sender();

        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();

        let node_for_seed = node.clone();
        let did_for_seed = node_did.clone();
        let seed_handle = tokio::spawn(async move {
            let request_id = wait_for_pending_request(&node_for_seed, &did_for_seed).await;
            let tool_doc = seed_tool_call(
                &node_for_seed,
                &request_id,
                "call-1",
                "agent_new",
                "running",
                "",
            )
            .await;
            let child = seed_child_request(
                &node_for_seed,
                &request_id,
                &tool_doc,
                "child-1",
                "processing",
            )
            .await;
            tokio::time::sleep(Duration::from_millis(600)).await;
            complete_tool_call(&node_for_seed, &tool_doc, "file contents").await;
            complete_child_request(&node_for_seed, &child).await;
            seed_assistant_message(&node_for_seed, &request_id, 1, "the answer").await;
            terminalize_request(&node_for_seed, &request_id, "completed").await;
        });

        let result = tokio::time::timeout(
            Duration::from_secs(30),
            manager.handle_prompt(prompt, &sender, &engine),
        )
        .await
        .expect("prompt should resolve within timeout")
        .expect("prompt should succeed");
        assert_eq!(result["stopReason"], json!("end_turn"));
        seed_handle.await.expect("seed task");

        let kinds = buffered_update_kinds(&buffer).await;
        assert_eq!(
            kinds.first().map(String::as_str),
            Some("user_message_chunk")
        );
        for expected in [
            "tool_call",
            "tool_call_update",
            "available_commands_update",
            "subagent_spawned",
            "subagent_progress",
            "subagent_finished",
            "agent_message_chunk",
        ] {
            assert!(
                kinds.iter().any(|kind| kind == expected),
                "expected a {expected} notification, got: {kinds:?}"
            );
        }
        for kind in [
            "tool_call",
            "available_commands_update",
            "subagent_spawned",
            "subagent_finished",
            "agent_message_chunk",
        ] {
            let count = kinds.iter().filter(|k| k == &kind).count();
            assert_eq!(count, 1, "{kind} must stream exactly once, got {count}");
        }
        let updates = parse_buffered_updates(&buffer).await;
        let revisions: Vec<_> = updates
            .iter()
            .filter_map(|event| {
                let update = event.pointer("/params/update")?;
                (update.get("sessionUpdate").and_then(Value::as_str) == Some("tool_call_update"))
                    .then_some(update)
            })
            .collect();
        assert_eq!(
            revisions,
            vec![
                &json!({
                    "sessionUpdate": "tool_call_update", "toolCallId": "call-1",
                    "content": [{"type": "content", "content": {"type": "text", "text": "file contents"}}],
                    "rawOutput": {"output": "file contents"},
                }),
                &json!({
                    "sessionUpdate": "tool_call_update", "toolCallId": "call-1",
                    "status": "completed",
                }),
            ]
        );
        assert!(
            updates.len() >= 8,
            "expected the full stream, got {updates:?}"
        );
        for update in &updates {
            let kind = update["params"]["update"]["sessionUpdate"]
                .as_str()
                .expect("sessionUpdate kind");
            let expected_method = if kind.starts_with("subagent_") {
                "_x.ai/session_notification"
            } else {
                "session/update"
            };
            assert_eq!(update["method"], expected_method);
            assert_eq!(update["params"]["sessionId"], "session-1");
            assert_eq!(update["params"]["_meta"]["promptId"], "prompt-1");
            assert!(
                update["params"]["_meta"]["eventId"].as_str().is_some(),
                "every notification carries an eventId"
            );
        }
    }

    #[tokio::test]
    async fn identical_text_distinct_rows_both_stream() {
        let (_tempdir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
        let engine = test_engine(node.clone());
        let (buffer, sender) = buffer_sender();

        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();

        let node_for_seed = node.clone();
        let did_for_seed = node_did.clone();
        let seed_handle = tokio::spawn(async move {
            let request_id = wait_for_pending_request(&node_for_seed, &did_for_seed).await;
            seed_assistant_message(&node_for_seed, &request_id, 1, "same text").await;
            seed_assistant_message(&node_for_seed, &request_id, 2, "same text").await;
            terminalize_request(&node_for_seed, &request_id, "completed").await;
        });

        let result = tokio::time::timeout(
            Duration::from_secs(30),
            manager.handle_prompt(prompt, &sender, &engine),
        )
        .await
        .expect("prompt should resolve within timeout")
        .expect("prompt should succeed");
        assert_eq!(result["stopReason"], json!("end_turn"));
        seed_handle.await.expect("seed task");

        let updates = parse_buffered_updates(&buffer).await;
        let chunks: Vec<&str> = updates
            .iter()
            .filter(|update| update["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
            .filter_map(|update| update["params"]["update"]["content"]["text"].as_str())
            .collect();
        assert_eq!(
            chunks,
            vec!["same text", "same text"],
            "both distinct rows must stream even with identical text"
        );
    }

    #[tokio::test]
    async fn outbound_close_after_submission_interrupts_and_drains() {
        let (_tempdir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let engine = test_engine(node.clone());
        let (frames_tx, frames_rx) = tokio::sync::mpsc::unbounded_channel::<
            crate::commands::grok_shim::protocol::ServerEnvelope,
        >();
        let sender = PromptSender::Live {
            outbound: AcpOutbound::for_frames(frames_tx),
        };

        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();

        let mut frames_rx = frames_rx;
        let outbound_closed = Arc::new(tokio::sync::Notify::new());
        let closed_signal = outbound_closed.clone();
        let closer = tokio::spawn(async move {
            let _ = frames_rx.recv().await;
            drop(frames_rx);
            closed_signal.notify_one();
        });
        let node_for_seed = node.clone();
        let did_for_seed = node_did.clone();
        let seed_handle = tokio::spawn(async move {
            let request_id = wait_for_pending_request(&node_for_seed, &did_for_seed).await;
            outbound_closed.notified().await;
            let _tool_doc = seed_tool_call(
                &node_for_seed,
                &request_id,
                "call-1",
                "read_file",
                "running",
                "",
            )
            .await;
        });

        let result = tokio::time::timeout(
            Duration::from_secs(30),
            manager.handle_prompt(prompt, &sender, &engine),
        )
        .await
        .expect("prompt should resolve within timeout");
        assert!(result.is_err(), "a closed outbound must fail the turn");
        closer.await.expect("closer task");
        seed_handle.await.expect("seed task");

        tokio::time::sleep(Duration::from_millis(200)).await;
        let query = r#"{ AgentRequest { request_id interrupt_requested_at } }"#;
        let response = node.execute(query).await;
        ensure_no_errors(&response, "test request query").expect("query");
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            rows.iter().any(|row| row
                .get("interrupt_requested_at")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())),
            "the submitted request must be interrupted after the mid-turn send failure"
        );

        let second = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("again")],
                "_meta": {"promptId": "prompt-2"},
            }),
            Some(json!(2)),
        )
        .unwrap();
        let node_for_terminalize = node.clone();
        let did_for_terminalize = node_did.clone();
        let terminalize_handle = tokio::spawn(async move {
            loop {
                let scope = gents::session::session_scope_filter(
                    &did_for_terminalize,
                    "session-1",
                    Some(&did_for_terminalize),
                );
                let query = format!(
                    "{{AgentRequest(filter: {{{scope}, lifecycle_state: {{_eq: \"pending\"}}}}) {{{}}}}}",
                    gents::SIGNED_REQUEST_FIELDS
                );
                let response = node_for_terminalize.execute(&query).await;
                ensure_no_errors(&response, "pending fixture rows").unwrap();
                let rows = response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .and_then(Value::as_array)
                    .cloned()
                    .expect("pending fixture rows");
                for row in &rows {
                    let receipt: gents_protocol::row::AgentRequestRow =
                        serde_json::from_value(row.clone()).unwrap();
                    terminalize_request(&node_for_terminalize, &receipt, "interrupted").await;
                }
                if rows.is_empty() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        });
        let (buffer2, sender2) = buffer_sender();
        let second_result = tokio::time::timeout(
            Duration::from_secs(30),
            manager.handle_prompt(second, &sender2, &engine),
        )
        .await
        .expect("second prompt should resolve within timeout")
        .expect("second prompt must be accepted after the drain");
        drop(buffer2);
        terminalize_handle.abort();
        assert_eq!(second_result["stopReason"], json!("cancelled"));
    }

    async fn wait_for_pending_request(
        node: &Arc<EmbeddedNode>,
        node_did: &str,
    ) -> gents_protocol::row::AgentRequestRow {
        let scope = gents::session::session_scope_filter(node_did, "session-1", Some(node_did));
        loop {
            let response = node.execute(&format!("{{AgentRequest(filter: {{{scope}, lifecycle_state: {{_eq: \"pending\"}}}}) {{{}}}}}", gents::SIGNED_REQUEST_FIELDS)).await;
            ensure_no_errors(&response, "wait for fixture request").unwrap();
            let rows = response
                .data
                .as_ref()
                .and_then(|data| data["AgentRequest"].as_array())
                .expect("request rows");
            assert!(rows.len() <= 1, "ambiguous pending fixture request");
            if let Some(row) = rows.first() {
                let row: gents_protocol::row::AgentRequestRow =
                    serde_json::from_value(row.clone()).unwrap();
                assert!(row.doc_id.is_some());
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn cancel_before_request_id_resolves_cancelled_and_permits_reuse() {
        let (_tempdir, node, node_did) = test_node().await;
        let submission_arrived = Arc::new(tokio::sync::Notify::new());
        let submission_release = Arc::new(tokio::sync::Notify::new());
        let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let graphql = spawn_gated_mock_graphql(
            node.clone(),
            Some((
                submission_arrived.clone(),
                submission_release.clone(),
                gate_armed,
            )),
        )
        .await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let engine = test_engine(node.clone());
        let (_buffer, sender) = buffer_sender();

        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();

        let prompt_handle = tokio::spawn({
            let manager = manager.clone();
            let sender = sender.clone();
            let engine = engine.clone();
            async move { manager.handle_prompt(prompt, &sender, &engine).await }
        });

        tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
            .await
            .expect("submission should arrive at the gated endpoint");

        let cancel = parse_cancel_notification(&json!({
            "sessionId": "session-1",
            "_meta": {"cancelSubagents": true, "promptId": "prompt-1"},
        }))
        .unwrap();
        manager
            .handle_cancel(cancel)
            .await
            .expect("cancel should succeed");

        submission_release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
            .await
            .expect("prompt should resolve within timeout")
            .expect("prompt task")
            .expect("cancel-before-id must resolve cancelled, not error");

        assert_eq!(result["stopReason"], json!("cancelled"));

        let second = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("again")],
                "_meta": {"promptId": "prompt-2"},
            }),
            Some(json!(2)),
        )
        .unwrap();
        let node_for_terminalize = node.clone();
        let did_for_terminalize = node_did.clone();
        let terminalize_handle = tokio::spawn(async move {
            loop {
                let scope = gents::session::session_scope_filter(
                    &did_for_terminalize,
                    "session-1",
                    Some(&did_for_terminalize),
                );
                let query = format!(
                    "{{AgentRequest(filter: {{{scope}, lifecycle_state: {{_eq: \"pending\"}}}}) {{{}}}}}",
                    gents::SIGNED_REQUEST_FIELDS
                );
                let response = node_for_terminalize.execute(&query).await;
                ensure_no_errors(&response, "pending fixture rows").unwrap();
                let rows = response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .and_then(Value::as_array)
                    .cloned()
                    .expect("pending fixture rows");
                for row in &rows {
                    let receipt: gents_protocol::row::AgentRequestRow =
                        serde_json::from_value(row.clone()).unwrap();
                    terminalize_request(&node_for_terminalize, &receipt, "interrupted").await;
                }
                if rows.is_empty() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        });
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            manager.handle_prompt(second, &sender, &engine),
        )
        .await
        .expect("second prompt should resolve within timeout")
        .expect("second prompt should succeed");
        assert_eq!(result["stopReason"], json!("cancelled"));
        terminalize_handle.abort();
    }

    #[tokio::test]
    async fn cancelled_submission_cannot_capture_an_exact_key_replacement() {
        let (_tempdir, node, node_did) = test_node().await;
        let submission_arrived = Arc::new(tokio::sync::Notify::new());
        let submission_release = Arc::new(tokio::sync::Notify::new());
        let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let graphql = spawn_gated_mock_graphql(
            node.clone(),
            Some((
                submission_arrived.clone(),
                submission_release.clone(),
                gate_armed,
            )),
        )
        .await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let engine = test_engine(node.clone());
        let (_buffer, sender) = buffer_sender();

        let prompt = || {
            parse_prompt_request(
                &json!({
                    "sessionId": "session-reuse",
                    "prompt": [text_block("hello")],
                    "_meta": {"promptId": "same-prompt"},
                }),
                Some(json!(1)),
            )
            .expect("prompt")
        };
        let first = tokio::spawn({
            let manager = manager.clone();
            let sender = sender.clone();
            let engine = engine.clone();
            let prompt = prompt();
            async move { manager.handle_prompt(prompt, &sender, &engine).await }
        });
        tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
            .await
            .expect("first submission reached gate");

        let cancel = parse_cancel_notification(&json!({
            "sessionId": "session-reuse",
            "_meta": {"promptId": "same-prompt"},
        }))
        .expect("cancel");
        manager.handle_cancel(cancel).await.expect("cancel first");

        let replacement = tokio::spawn({
            let manager = manager.clone();
            let sender = sender.clone();
            let engine = engine.clone();
            let prompt = prompt();
            async move { manager.handle_prompt(prompt, &sender, &engine).await }
        });
        let replacement_request_id = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let request_id = manager
                    .state
                    .lock()
                    .await
                    .entries
                    .get(&("session-reuse".to_string(), "same-prompt".to_string()))
                    .and_then(|entry| entry.request.as_ref().map(|row| row.request_id.clone()));
                if let Some(request_id) = request_id {
                    break request_id;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("replacement request id registered");

        submission_release.notify_one();
        let first_result = tokio::time::timeout(Duration::from_secs(30), first)
            .await
            .expect("old prompt resolves")
            .expect("old prompt task")
            .expect("old prompt result");
        assert_eq!(first_result["stopReason"], json!("cancelled"));
        assert_eq!(
            manager
                .state
                .lock()
                .await
                .entries
                .get(&("session-reuse".to_string(), "same-prompt".to_string()))
                .and_then(|entry| entry.request.as_ref().map(|row| row.request_id.as_str())),
            Some(replacement_request_id.as_str()),
            "the old generation must not overwrite the exact-key replacement"
        );

        let cancel_replacement = parse_cancel_notification(&json!({
            "sessionId": "session-reuse",
            "_meta": {"promptId": "same-prompt"},
        }))
        .expect("cancel replacement");
        manager
            .handle_cancel(cancel_replacement)
            .await
            .expect("cancel replacement");
        let replacement_result = tokio::time::timeout(Duration::from_secs(30), replacement)
            .await
            .expect("replacement resolves")
            .expect("replacement task")
            .expect("replacement result");
        assert_eq!(replacement_result["stopReason"], json!("cancelled"));
    }

    #[tokio::test]
    async fn failed_submission_cannot_remove_an_exact_key_replacement() {
        let (_tempdir, node, node_did) = test_node().await;
        let submission_arrived = Arc::new(tokio::sync::Notify::new());
        let submission_release = Arc::new(tokio::sync::Notify::new());
        let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let submission_fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let graphql = spawn_gated_mock_graphql_with_failure(
            node.clone(),
            Some((
                submission_arrived.clone(),
                submission_release.clone(),
                gate_armed,
            )),
            Some(submission_fail),
        )
        .await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let engine = test_engine(node.clone());
        let (_buffer, sender) = buffer_sender();
        let prompt = || {
            parse_prompt_request(
                &json!({
                    "sessionId": "session-failed-reuse",
                    "prompt": [text_block("hello")],
                    "_meta": {"promptId": "same-prompt"},
                }),
                Some(json!(1)),
            )
            .expect("prompt")
        };

        let first = tokio::spawn({
            let manager = manager.clone();
            let sender = sender.clone();
            let engine = engine.clone();
            let prompt = prompt();
            async move { manager.handle_prompt(prompt, &sender, &engine).await }
        });
        tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
            .await
            .expect("first submission reached gate");

        manager
            .handle_cancel(
                parse_cancel_notification(&json!({
                    "sessionId": "session-failed-reuse",
                    "_meta": {"promptId": "same-prompt"},
                }))
                .expect("cancel first"),
            )
            .await
            .expect("cancel first");

        let replacement = tokio::spawn({
            let manager = manager.clone();
            let sender = sender.clone();
            let engine = engine.clone();
            let prompt = prompt();
            async move { manager.handle_prompt(prompt, &sender, &engine).await }
        });
        let key = (
            "session-failed-reuse".to_string(),
            "same-prompt".to_string(),
        );
        let replacement_request_id = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(request_id) = manager
                    .state
                    .lock()
                    .await
                    .entries
                    .get(&key)
                    .and_then(|entry| entry.request.as_ref().map(|row| row.request_id.clone()))
                {
                    break request_id;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("replacement request id registered");

        submission_release.notify_one();
        let first_result = tokio::time::timeout(Duration::from_secs(30), first)
            .await
            .expect("old prompt resolves")
            .expect("old prompt task")
            .expect("old failed submission resolves as cancelled");
        assert_eq!(first_result["stopReason"], json!("cancelled"));
        assert_eq!(
            manager
                .state
                .lock()
                .await
                .entries
                .get(&key)
                .and_then(|entry| entry.request.as_ref().map(|row| row.request_id.as_str())),
            Some(replacement_request_id.as_str()),
            "the failed old generation must not remove its replacement"
        );

        manager
            .handle_cancel(
                parse_cancel_notification(&json!({
                    "sessionId": "session-failed-reuse",
                    "_meta": {"promptId": "same-prompt"},
                }))
                .expect("cancel replacement"),
            )
            .await
            .expect("cancel replacement");
        let replacement_result = tokio::time::timeout(Duration::from_secs(30), replacement)
            .await
            .expect("replacement resolves")
            .expect("replacement task")
            .expect("replacement result");
        assert_eq!(replacement_result["stopReason"], json!("cancelled"));
    }

    #[tokio::test]
    async fn cancel_selection_cannot_drain_an_exact_key_replacement() {
        let (_tempdir, node, node_did) = test_node().await;
        let manager = Arc::new(TurnManager::new(
            node,
            test_config("http://127.0.0.1:1/".to_string(), &node_did),
        ));
        let key = ("session-reuse".to_string(), "same-prompt".to_string());
        let old_generation = Arc::new(Mutex::new(CancelBeforeIdLatch::default()));
        let (old_tx, mut old_rx) = oneshot::channel::<Result<Value>>();
        manager.state.lock().await.entries.insert(
            key.clone(),
            PendingPrompt {
                response_tx: Some(old_tx),
                request: Some(
                    serde_json::from_value(json!({"request_id": "old-request"})).unwrap(),
                ),
                cancel_before_id: old_generation,
                drained: false,
            },
        );

        let gate = TestGate {
            arrived: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        *manager.cancel_selection_gate.lock().await = Some(gate.clone());
        let cancel = parse_cancel_notification(&json!({
            "sessionId": "session-reuse",
            "_meta": {"promptId": "same-prompt"},
        }))
        .expect("cancel");
        let cancel_handle = tokio::spawn({
            let manager = manager.clone();
            async move { manager.handle_cancel(cancel).await }
        });
        tokio::time::timeout(Duration::from_secs(30), gate.arrived.notified())
            .await
            .expect("cancel should snapshot the old generation");

        let replacement_generation = Arc::new(Mutex::new(CancelBeforeIdLatch::default()));
        let (replacement_tx, mut replacement_rx) = oneshot::channel::<Result<Value>>();
        {
            let mut state = manager.state.lock().await;
            let mut old = state.entries.remove(&key).expect("old generation");
            old.resolve(Ok(json!({"stopReason": "end_turn"})));
            state.entries.insert(
                key.clone(),
                PendingPrompt {
                    response_tx: Some(replacement_tx),
                    request: Some(
                        serde_json::from_value(json!({"request_id": "replacement-request"}))
                            .unwrap(),
                    ),
                    cancel_before_id: replacement_generation.clone(),
                    drained: false,
                },
            );
        }
        assert_eq!(
            old_rx
                .try_recv()
                .expect("old generation terminal response")
                .expect("old generation result")["stopReason"],
            json!("end_turn")
        );

        gate.release.notify_one();
        tokio::time::timeout(Duration::from_secs(30), cancel_handle)
            .await
            .expect("cancel should finish")
            .expect("cancel task")
            .expect("cancel result");

        {
            let state = manager.state.lock().await;
            let replacement = state.entries.get(&key).expect("replacement survives");
            assert!(Arc::ptr_eq(
                &replacement.cancel_before_id,
                &replacement_generation
            ));
            assert_eq!(
                replacement
                    .request
                    .as_ref()
                    .map(|row| row.request_id.as_str()),
                Some("replacement-request")
            );
            assert!(!replacement.drained);
        }
        assert!(matches!(
            replacement_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        let mut replacement = manager
            .state
            .lock()
            .await
            .entries
            .remove(&key)
            .expect("replacement cleanup");
        replacement.resolve(Ok(json!({"stopReason": "cancelled"})));
        assert_eq!(
            replacement_rx
                .try_recv()
                .expect("replacement cleanup response")
                .expect("replacement cleanup result")["stopReason"],
            json!("cancelled")
        );
    }

    #[tokio::test]
    async fn disconnect_before_request_id_resolves_cancelled() {
        let (_tempdir, node, node_did) = test_node().await;
        let submission_arrived = Arc::new(tokio::sync::Notify::new());
        let submission_release = Arc::new(tokio::sync::Notify::new());
        let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let graphql = spawn_gated_mock_graphql(
            node.clone(),
            Some((
                submission_arrived.clone(),
                submission_release.clone(),
                gate_armed,
            )),
        )
        .await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let engine = test_engine(node.clone());
        let (_buffer, sender) = buffer_sender();

        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();

        let prompt_handle = tokio::spawn({
            let manager = manager.clone();
            let sender = sender.clone();
            let engine = engine.clone();
            async move { manager.handle_prompt(prompt, &sender, &engine).await }
        });

        tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
            .await
            .expect("submission should arrive at the gated endpoint");

        manager
            .handle_disconnect()
            .await
            .expect("disconnect should succeed");

        submission_release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
            .await
            .expect("prompt should resolve within timeout")
            .expect("prompt task")
            .expect("disconnect-before-id must resolve cancelled, not error");
        assert_eq!(result["stopReason"], json!("cancelled"));
    }

    #[tokio::test]
    async fn disconnect_parked_before_latch_with_finishing_submission_failure_resolves_cancelled() {
        let (_tempdir, node, node_did) = test_node().await;
        let submission_arrived = Arc::new(tokio::sync::Notify::new());
        let submission_release = Arc::new(tokio::sync::Notify::new());
        let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let submission_fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let graphql = spawn_gated_mock_graphql_with_failure(
            node.clone(),
            Some((
                submission_arrived.clone(),
                submission_release.clone(),
                gate_armed,
            )),
            Some(submission_fail),
        )
        .await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let engine = test_engine(node.clone());
        let (_buffer, sender) = buffer_sender();

        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();

        let prompt_handle = tokio::spawn({
            let manager = manager.clone();
            let sender = sender.clone();
            let engine = engine.clone();
            async move { manager.handle_prompt(prompt, &sender, &engine).await }
        });

        tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
            .await
            .expect("submission should arrive at the gated endpoint");

        let gate = TestGate {
            arrived: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        *manager.disconnect_gate.lock().await = Some(gate.clone());
        let manager_for_disconnect = manager.clone();
        let disconnect_handle = tokio::spawn(async move {
            manager_for_disconnect
                .handle_disconnect()
                .await
                .expect("disconnect should succeed")
        });

        tokio::time::timeout(Duration::from_secs(30), gate.arrived.notified())
            .await
            .expect("disconnect should park at the seam");

        {
            let state = manager.state.lock().await;
            assert!(state.closed, "the disconnect has published closed");
            assert!(
                state.entries.is_empty(),
                "the disconnect has drained the entry table"
            );
        }

        submission_release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
            .await
            .expect("prompt should resolve within timeout")
            .expect("prompt task")
            .expect("submission failure in the closed-drained window must resolve cancelled");
        assert_eq!(
            result["stopReason"],
            json!("cancelled"),
            "the disconnect's required stopReason wins over the GraphQL error"
        );

        gate.release.notify_one();
        tokio::time::timeout(Duration::from_secs(30), disconnect_handle)
            .await
            .expect("disconnect should resolve within timeout")
            .expect("disconnect task");

        {
            let state = manager.state.lock().await;
            assert!(state.entries.is_empty());
            assert!(state.closed);
        }

        let query = r#"{ AgentRequest { request_id } }"#;
        let response = node.execute(query).await;
        ensure_no_errors(&response, "test request query").expect("query");
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            rows.is_empty(),
            "a submission failed into a closed connection must not mint an AgentRequest, got {rows:?}"
        );
    }

    #[tokio::test]
    async fn cancel_parked_before_latch_with_finishing_submission_failure_resolves_cancelled() {
        let (_tempdir, node, node_did) = test_node().await;
        let submission_arrived = Arc::new(tokio::sync::Notify::new());
        let submission_release = Arc::new(tokio::sync::Notify::new());
        let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let submission_fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let graphql = spawn_gated_mock_graphql_with_failure(
            node.clone(),
            Some((
                submission_arrived.clone(),
                submission_release.clone(),
                gate_armed,
            )),
            Some(submission_fail),
        )
        .await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let engine = test_engine(node.clone());
        let (_buffer, sender) = buffer_sender();

        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();

        let prompt_handle = tokio::spawn({
            let manager = manager.clone();
            let sender = sender.clone();
            let engine = engine.clone();
            async move { manager.handle_prompt(prompt, &sender, &engine).await }
        });

        tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
            .await
            .expect("submission should arrive at the gated endpoint");

        let gate = TestGate {
            arrived: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        *manager.cancel_drain_gate.lock().await = Some(gate.clone());
        let cancel = parse_cancel_notification(&json!({
            "sessionId": "session-1",
            "_meta": {"cancelSubagents": true, "promptId": "prompt-1"},
        }))
        .unwrap();
        let manager_for_cancel = manager.clone();
        let cancel_handle = tokio::spawn(async move {
            manager_for_cancel
                .handle_cancel(cancel)
                .await
                .expect("cancel should succeed")
        });

        tokio::time::timeout(Duration::from_secs(30), gate.arrived.notified())
            .await
            .expect("cancel should park at the seam");

        {
            let state = manager.state.lock().await;
            assert!(
                !state.closed,
                "an explicit cancel leaves the connection open"
            );
            assert!(
                state.entries.is_empty(),
                "the cancel has removed the pending entry"
            );
        }

        submission_release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
            .await
            .expect("prompt should resolve within timeout")
            .expect("prompt task")
            .expect("submission failure in the cancel-drain window must resolve cancelled");
        assert_eq!(
            result["stopReason"],
            json!("cancelled"),
            "the explicit cancel's required stopReason wins over the GraphQL error"
        );

        gate.release.notify_one();
        tokio::time::timeout(Duration::from_secs(30), cancel_handle)
            .await
            .expect("cancel should resolve within timeout")
            .expect("cancel task");

        {
            let state = manager.state.lock().await;
            assert!(state.entries.is_empty());
            assert!(!state.closed);
        }

        let query = r#"{ AgentRequest { request_id } }"#;
        let response = node.execute(query).await;
        ensure_no_errors(&response, "test request query").expect("query");
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            rows.is_empty(),
            "a submission failed into a cancelled turn must not mint an AgentRequest, got {rows:?}"
        );
    }

    #[tokio::test]
    async fn prompt_gated_before_insertion_is_rejected_after_disconnect() {
        let (_tempdir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let engine = test_engine(node.clone());
        let (_buffer, sender) = buffer_sender();

        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();

        let gate = TestGate {
            arrived: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        *manager.insertion_gate.lock().await = Some(gate.clone());

        let manager_for_prompt = manager.clone();
        let sender_for_prompt = sender.clone();
        let engine_for_prompt = engine.clone();
        let prompt_handle = tokio::spawn(async move {
            manager_for_prompt
                .handle_prompt(prompt, &sender_for_prompt, &engine_for_prompt)
                .await
        });

        tokio::time::timeout(Duration::from_secs(30), gate.arrived.notified())
            .await
            .expect("prompt should arrive at the insertion gate");

        manager
            .handle_disconnect()
            .await
            .expect("disconnect should succeed");

        gate.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
            .await
            .expect("prompt should resolve within timeout")
            .expect("prompt task");
        let error = result.expect_err("a prompt gated across disconnect must be rejected");
        assert!(
            error.to_string().contains("already disconnected"),
            "rejection must name the closed connection, got: {error:#}"
        );

        {
            let state = manager.state.lock().await;
            assert!(
                state.entries.is_empty(),
                "no entry may be inserted after the closed latch"
            );
            assert!(state.closed, "the connection stays closed");
        }

        let query = r#"{ AgentRequest { request_id } }"#;
        let response = node.execute(query).await;
        ensure_no_errors(&response, "test request query").expect("query");
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            rows.is_empty(),
            "a prompt rejected by the closed latch must never create an AgentRequest, got {rows:?}"
        );

        manager
            .handle_disconnect()
            .await
            .expect("duplicate disconnect should succeed");
        {
            let state = manager.state.lock().await;
            assert!(state.entries.is_empty());
            assert!(state.closed);
        }
    }

    #[tokio::test]
    async fn disconnect_drains_an_already_pending_entry_and_duplicate_disconnect_is_a_noop() {
        let (_tempdir, node, node_did) = test_node().await;
        let submission_arrived = Arc::new(tokio::sync::Notify::new());
        let submission_release = Arc::new(tokio::sync::Notify::new());
        let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let graphql = spawn_gated_mock_graphql(
            node.clone(),
            Some((
                submission_arrived.clone(),
                submission_release.clone(),
                gate_armed,
            )),
        )
        .await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let engine = test_engine(node.clone());
        let (_buffer, sender) = buffer_sender();

        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();

        let prompt_handle = tokio::spawn({
            let manager = manager.clone();
            let sender = sender.clone();
            let engine = engine.clone();
            async move { manager.handle_prompt(prompt, &sender, &engine).await }
        });

        tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
            .await
            .expect("submission should arrive at the gated endpoint");

        {
            let state = manager.state.lock().await;
            assert_eq!(state.entries.len(), 1, "the prompt entry must be pending");
        }
        manager
            .handle_disconnect()
            .await
            .expect("disconnect should succeed");
        {
            let state = manager.state.lock().await;
            assert!(
                state.entries.is_empty(),
                "disconnect must drain every entry"
            );
            assert!(state.closed);
        }

        manager
            .handle_disconnect()
            .await
            .expect("duplicate disconnect should succeed");
        {
            let state = manager.state.lock().await;
            assert!(state.entries.is_empty());
            assert!(state.closed);
        }

        submission_release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
            .await
            .expect("prompt should resolve within timeout")
            .expect("prompt task")
            .expect("the drained prompt must resolve cancelled, not error");
        assert_eq!(result["stopReason"], json!("cancelled"));
    }

    #[tokio::test]
    async fn send_failure_after_submission_interrupts_the_request() {
        let (_tempdir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = TurnManager::new(node.clone(), test_config(graphql, &node_did));
        let engine = test_engine(node.clone());
        let (frames_tx, frames_rx) = tokio::sync::mpsc::unbounded_channel::<
            crate::commands::grok_shim::protocol::ServerEnvelope,
        >();
        drop(frames_rx);
        let sender = PromptSender::Live {
            outbound: AcpOutbound::for_frames(frames_tx),
        };

        let prompt = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("hello")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();

        let result = tokio::time::timeout(
            Duration::from_secs(30),
            manager.handle_prompt(prompt, &sender, &engine),
        )
        .await
        .expect("prompt should resolve within timeout");
        assert!(
            result.is_err(),
            "closed outbound must surface a send failure"
        );

        tokio::time::sleep(Duration::from_millis(200)).await;
        let query = r#"{ AgentRequest { request_id interrupt_requested_at } }"#;
        let response = node.execute(query).await;
        ensure_no_errors(&response, "test request query").expect("query");
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            rows.iter().any(|row| row
                .get("interrupt_requested_at")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())),
            "the submitted request must be interrupted after the send failure"
        );
    }

    #[tokio::test]
    async fn second_prompt_for_live_session_is_rejected() {
        let (_tempdir, node, node_did) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = Arc::new(TurnManager::new(
            node.clone(),
            test_config(graphql, &node_did),
        ));
        let projections = test_engine(node.clone());
        let (buffer, sender) = buffer_sender();

        let first = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("first")],
                "_meta": {"promptId": "prompt-1"},
            }),
            Some(json!(1)),
        )
        .unwrap();
        let manager_for_first = manager.clone();
        let sender_for_first = sender.clone();
        let engine_for_first = projections.clone();
        let first_handle = tokio::spawn(async move {
            manager_for_first
                .handle_prompt(first, &sender_for_first, &engine_for_first)
                .await
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if !buffer.lock().await.is_empty() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "first prompt never echoed; pending entry never went live"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let second = parse_prompt_request(
            &json!({
                "sessionId": "session-1",
                "prompt": [text_block("second")],
                "_meta": {"promptId": "prompt-2"},
            }),
            Some(json!(2)),
        )
        .unwrap();
        let rejection = manager
            .handle_prompt(second, &sender, &projections)
            .await
            .expect_err("second prompt for a live session must be rejected");
        assert!(
            rejection.to_string().contains("live prompt"),
            "rejection must name the one-pending-per-session rule, got: {rejection}"
        );

        let node_for_terminalize = node.clone();
        let did_for_terminalize = node_did.clone();
        let terminalize_handle = tokio::spawn(async move {
            loop {
                let scope = gents::session::session_scope_filter(
                    &did_for_terminalize,
                    "session-1",
                    Some(&did_for_terminalize),
                );
                let query = format!(
                    "{{AgentRequest(filter: {{{scope}, lifecycle_state: {{_eq: \"pending\"}}}}) {{{}}}}}",
                    gents::SIGNED_REQUEST_FIELDS
                );
                let response = node_for_terminalize.execute(&query).await;
                ensure_no_errors(&response, "pending fixture rows").unwrap();
                let rows = response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .and_then(Value::as_array)
                    .cloned()
                    .expect("pending fixture rows");
                assert!(rows.len() <= 1, "ambiguous pending fixture request");
                if let Some(row) = rows.first() {
                    let receipt: gents_protocol::row::AgentRequestRow =
                        serde_json::from_value(row.clone()).unwrap();
                    terminalize_request(&node_for_terminalize, &receipt, "completed").await;
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        let first_result = tokio::time::timeout(Duration::from_secs(30), first_handle)
            .await
            .expect("first prompt should resolve within timeout")
            .expect("first prompt task")
            .expect("first prompt should succeed");
        assert_eq!(first_result["stopReason"], json!("end_turn"));
        terminalize_handle.await.expect("terminalize task");
    }
}
