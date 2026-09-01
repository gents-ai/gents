//! Grok shim prompt/cancel turn manager.
//!
//! This leaf owns the connection-scoped pending prompt lifecycle for the Grok
//! pager's `session/prompt` and `session/cancel` wire methods.
//!
//! `session/prompt` parses the audited wire shape (sessionId, prompt blocks
//! with per-block `meta.skillTokenRanges` / PromptBlockMeta::bash, and
//! `_meta.promptId` / `_meta.screenMode` / `_meta.sendNow`), echoes the user
//! message back as a `user_message_chunk` `session/update` notification, and
//! then defers the JSON-RPC response until the durable request terminalizes.
//! The response result is a `stopReason` projection of the durable lifecycle —
//! never a persisted field.
//!
//! `session/cancel` parses the audited notification shape (sessionId plus
//! `_meta.cancelSubagents` / `_meta.cancelTrigger` / `_meta.rewindIfNoOutput`
//! / `_meta.rewindIfPristine` / `_meta.promptId`), is a notification (never
//! responded to), and interrupts the pending request through
//! [`gents::interrupt_request`]. `cancelSubagents=true` also interrupts
//! runtime child `AgentRequest` rows linked by `caused_by_parent_request_id`;
//! static `Task` configuration rows are never queried or mutated as runtime
//! state.
//!
//! Ordering contract: the returned Gents request id is registered on the
//! pending entry *before* the first fallible outbound send, so a send failure
//! after submission interrupts the durable request instead of leaking it.
//! Cancel/disconnect may fire before the request id is even known — in that
//! window they drain the pending entry, resolve the connected prompt with
//! `stopReason="cancelled"`, and cancel any future submission of that prompt,
//! so the session immediately accepts the next prompt.
//!
//! One pending prompt per session: a second `session/prompt` for the same
//! session while one is live is rejected and does not disturb the live turn.
//! Pending prompts are keyed by (session id, prompt id) inside this
//! connection-scoped manager.
//!
//! All durable reads/writes go through the in-process embedded node
//! (`node.execute(&query).await`) with every interpolated value escaped by
//! `gents::graphql::escape_graphql_string`; no HTTP GraphQL helper is used
//! except the `create_agent_request` seam, which takes the bound GraphQL
//! endpoint. The turn does not stream durable `AgentMessage`/`AgentToolCall`
//! projection — that is owned by the projection leaves — so nothing here
//! replays a session or duplicates durable materialization.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use defra_node::EmbeddedNode;
use gents::graphql::{ensure_no_errors, escape_graphql_string};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};

/// JSON-RPC method names on the Grok pager wire.
pub(super) const SESSION_PROMPT_METHOD: &str = "session/prompt";
pub(super) const SESSION_CANCEL_METHOD: &str = "session/cancel";
pub(super) const SESSION_UPDATE_METHOD: &str = "session/update";

/// JSON-RPC error codes used for prompt-shaped failures.
pub(super) const JSONRPC_INVALID_REQUEST: i64 = -32600;
pub(super) const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
pub(super) const JSONRPC_INVALID_PARAMS: i64 = -32602;
pub(super) const JSONRPC_INTERNAL_ERROR: i64 = -32603;

/// Poll cadence for watching the durable request terminalize. The embedded
/// node exposes no subscription seam to the shim, so terminalization is
/// observed by bounded polling; the pager expects the deferred response
/// promptly after terminalization, so the interval is short.
const TERMINAL_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Wire values for `screenMode` accepted on `session/prompt` `_meta`.
pub(super) const SCREEN_MODES: [&str; 3] = ["fullscreen", "inline", "minimal"];

/// A single prompt content block as sent by the pager.
///
/// The audited wire carries `prompt: [{"type":"text","text":"...", ...}]` with
/// an optional block `meta` containing `skillTokenRanges` (array of
/// [start,end] pairs) or a PromptBlockMeta::bash command stamp.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PromptBlock {
    pub kind: String,
    pub text: String,
    pub meta: Option<Value>,
}

impl PromptBlock {
    /// The `meta.bash.command` stamp of the bash variant, when present.
    pub(super) fn bash_command(&self) -> Option<String> {
        let meta = self.meta.as_ref()?;
        let command = meta
            .get("bash")
            .and_then(|bash| bash.get("command"))
            .and_then(Value::as_str)?;
        (!command.is_empty()).then(|| command.to_string())
    }

    /// Whether this block carries non-empty `skillTokenRanges`.
    pub(super) fn has_skill_token_ranges(&self) -> bool {
        self.meta
            .as_ref()
            .and_then(|meta| meta.get("skillTokenRanges"))
            .and_then(Value::as_array)
            .is_some_and(|ranges| !ranges.is_empty())
    }
}

/// The audited `session/prompt` request shape.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PromptRequest {
    pub session_id: String,
    pub prompt: Vec<PromptBlock>,
    pub prompt_id: Option<String>,
    pub screen_mode: Option<String>,
    pub send_now: bool,
    pub id: Option<Value>,
}

/// The audited `session/cancel` notification shape.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct CancelNotification {
    pub session_id: String,
    pub cancel_subagents: bool,
    pub cancel_trigger: Option<String>,
    pub rewind_if_no_output: bool,
    pub rewind_if_pristine: bool,
    pub prompt_id: Option<String>,
}

impl CancelNotification {
    /// Build the audited `_meta` payload the pager sends with a cancel.
    pub(super) fn meta(&self) -> Value {
        let mut meta = json!({
            "cancelSubagents": self.cancel_subagents,
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

/// Projection of the durable terminal state into a wire `stopReason`.
///
/// `stopReason` is an adapter projection, not a persisted field: the durable
/// source is `AgentRequest.lifecycle_state` plus the `AgentResponse` status
/// vocabulary and its `interrupted_at` marker.
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

/// A notification the turn projected to the connected client.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ProjectedNotification {
    /// Serialized JSON-RPC line, exactly as written to the outbound channel.
    pub line: String,
}

/// How a turn sends notifications to the connected client.
///
/// Cloning shares the underlying channel/buffer, which lets a caller keep a
/// copy while a spawned task drives one turn.
#[derive(Clone)]
pub(super) enum PromptSender {
    /// Sends one JSON-RPC notification line to the live client. The sender
    /// is fallible: a closed channel must interrupt the submitted request.
    Line {
        connection_id: u64,
        outbound_tx: mpsc::UnboundedSender<String>,
    },
    /// Collects serialized notification lines in memory (tests, headless
    /// capture). The buffer never fails, so a test can simulate a send
    /// failure only through the Line variant.
    Buffer {
        buffer: Arc<Mutex<Vec<String>>>,
    },
}

impl PromptSender {
    /// Serialize and send one `session/update` notification with the audited
    /// payload shape. This is the first fallible outbound send after the
    /// request id is registered.
    async fn send_user_message_chunk(
        &self,
        session_id: &str,
        prompt_id: &str,
        block: &PromptBlock,
        prompt_index: usize,
    ) -> Result<()> {
        let params = json!({
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "user_message_chunk",
                "content": {
                    "type": "text",
                    "text": block.text,
                    "meta": {
                        "promptIndex": prompt_index,
                        "hideFromScrollback": false,
                    },
                },
            },
            "_meta": {
                "promptId": prompt_id,
                "isReplay": false,
            },
        });
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": SESSION_UPDATE_METHOD,
            "params": params,
        }))
        .context("serialize user_message_chunk session/update")?;
        match self {
            PromptSender::Line {
                connection_id,
                outbound_tx,
            } => outbound_tx
                .send(line.clone())
                .with_context(|| format!("connection {connection_id} outbound closed")),
            PromptSender::Buffer { buffer } => {
                buffer.lock().await.push(line);
                Ok(())
            }
        }
    }
}

/// Configuration the ACP service binds into the turn manager.
#[derive(Clone, Debug)]
pub(super) struct TurnManagerConfig {
    /// The agent did every request is submitted under.
    pub agent_did: String,
    /// The behavior id the serving shim is bound to.
    pub behavior_id: String,
    /// GraphQL endpoint string accepted by `create_agent_request`; the
    /// in-process embedded node is authoritative for reads.
    pub graphql: String,
}

/// Shared latch recorded the moment a prompt is cancelled before its Gents
/// request id is known. The submitter checks it after registration so the
/// cancel-before-id race resolves deterministically.
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

/// State of one connection-scoped pending prompt. The (session id, prompt id)
/// key carries the correlation ids; this struct carries only the response
/// plumbing and the submission state.
struct PendingPrompt {
    /// Resolves the deferred `session/prompt` response.
    response_tx: Option<oneshot::Sender<Result<Value>>>,
    /// The Gents request id once submission succeeded; registered here
    /// *before* the first fallible outbound send.
    request_id: Option<String>,
    /// Latch for the cancel-before-request-id window.
    cancel_before_id: Arc<Mutex<CancelBeforeIdLatch>>,
    /// Whether cancel/disconnect already drained this entry.
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

/// Owns the connection-scoped pending prompts and exposes the prompt, cancel,
/// and disconnect operations.
pub(super) struct TurnManager {
    node: Arc<EmbeddedNode>,
    config: TurnManagerConfig,
    pending: Mutex<HashMap<(String, String), PendingPrompt>>,
}

impl TurnManager {
    pub(super) fn new(node: Arc<EmbeddedNode>, config: TurnManagerConfig) -> Self {
        TurnManager {
            node,
            config,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Handle a `session/prompt` request.
    ///
    /// Returns the deferred `session/prompt` result value (`stopReason`).
    /// The ACP service wraps it in the JSON-RPC response envelope.
    pub(super) async fn handle_prompt(
        &self,
        request: PromptRequest,
        sender: &PromptSender,
    ) -> Result<Value> {
        if request.prompt.is_empty() {
            anyhow::bail!("session/prompt requires at least one prompt block");
        }
        let prompt_id = request
            .prompt_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let key = (request.session_id.clone(), prompt_id.clone());
        let (response_tx, response_rx) = oneshot::channel::<Result<Value>>();
        let cancel_before_id = Arc::new(Mutex::new(CancelBeforeIdLatch::default()));
        {
            let mut pending = self.pending.lock().await;
            if pending
                .keys()
                .any(|(session, _)| session == &request.session_id)
            {
                anyhow::bail!("session already has a live prompt");
            }
            pending.insert(
                key.clone(),
                PendingPrompt {
                    response_tx: Some(response_tx),
                    request_id: None,
                    cancel_before_id: cancel_before_id.clone(),
                    drained: false,
                },
            );
        }

        // Submit the durable request first. The returned request id is
        // registered on the pending entry *before* the first fallible
        // outbound send (the user echo below), so a send failure after
        // submission interrupts the request rather than leaking it.
        let submission = self.submit_request(&request, &prompt_id).await;
        let request_id = match submission {
            Ok(request_id) => {
                {
                    let mut pending = self.pending.lock().await;
                    if let Some(entry) = pending.get_mut(&key) {
                        entry.request_id = Some(request_id.clone());
                    }
                }
                request_id
            }
            Err(error) => {
                // Submission failed before any request id existed. If
                // cancel/disconnect drained the entry during the race window,
                // resolve cancelled; otherwise surface the submission error.
                let cancelled_before_id = cancel_before_id.lock().await.is_cancelled();
                self.remove_pending(&key).await;
                drop(response_rx);
                if cancelled_before_id {
                    tracing::info!(
                        %error,
                        session_id = %request.session_id,
                        prompt_id = %prompt_id,
                        "Grok shim prompt submission failed after cancel-before-id; resolving cancelled"
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

        // First fallible outbound send after registration: the user echo. If
        // cancel/disconnect drained the entry in the meantime, or the send
        // itself fails, interrupt immediately and resolve cancelled.
        let drained_during_submission = {
            let pending = self.pending.lock().await;
            pending.get(&key).is_none_or(|entry| entry.drained)
        };
        if drained_during_submission {
            self.interrupt_submitted(&request_id).await;
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
                .send_user_message_chunk(&request.session_id, &prompt_id, block, index)
                .await
            {
                // Send failure after submission: interrupt the durable
                // request, drain the entry, and surface the failure.
                self.interrupt_and_drain(&key, &request_id).await;
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

        // Deferred response: watch the durable request until it terminalizes
        // or the pending entry is drained by cancel/disconnect.
        let outcome = self
            .watch_terminal(&key, &request_id, response_rx)
            .await?;
        Ok(json!({"stopReason": outcome.wire_name()}))
    }

    /// Handle a `session/cancel` notification. Never sends a response.
    pub(super) async fn handle_cancel(&self, notification: CancelNotification) -> Result<()> {
        tracing::info!(
            session_id = %notification.session_id,
            cancel_subagents = notification.cancel_subagents,
            prompt_id = notification.prompt_id.as_deref().unwrap_or(""),
            "Grok shim received session/cancel"
        );
        let target_prompt_id = notification.prompt_id.clone();
        let keys: Vec<(String, String)> = {
            let pending = self.pending.lock().await;
            pending
                .keys()
                .filter(|(session, _)| session == &notification.session_id)
                .filter(|(_, prompt_id)| {
                    target_prompt_id
                        .as_deref()
                        .is_none_or(|expected| expected == prompt_id.as_str())
                })
                .cloned()
                .collect()
        };
        for key in keys {
            let request_id = self
                .drain_entry(&key, StopReason::Cancelled)
                .await;
            if let Some(request_id) = request_id {
                self.interrupt_submitted(&request_id).await;
                if notification.cancel_subagents {
                    self.interrupt_child_requests(&request_id).await;
                }
            }
        }
        Ok(())
    }

    /// Handle connection teardown: drain every pending prompt and interrupt
    /// their submitted requests. No response is ever sent (the channel is
    /// gone); the deferred response is resolved so a concurrently awaited
    /// prompt future observes the drain.
    pub(super) async fn handle_disconnect(&self) -> Result<()> {
        let mut pending = self.pending.lock().await;
        let drained: Vec<((String, String), Arc<Mutex<CancelBeforeIdLatch>>, Option<String>)> =
            pending
                .drain()
                .map(|(key, mut entry)| {
                    entry.resolve(Ok(json!({
                        "stopReason": StopReason::Cancelled.wire_name(),
                    })));
                    (
                        key,
                        entry.cancel_before_id.clone(),
                        entry.request_id.clone(),
                    )
                })
                .collect();
        drop(pending);
        for ((_session_id, prompt_id), latch, request_id) in drained {
            // Latch the cancel-before-id window for submitters that are still
            // inside `create_agent_request` and have not registered a request
            // id yet; they observe the latch and resolve cancelled.
            let _first_cancel = latch.lock().await.cancel();
            if let Some(request_id) = request_id {
                if let Err(error) = gents::interrupt_request(self.node.as_ref(), &request_id).await
                {
                    tracing::warn!(
                        %error,
                        prompt_id = %prompt_id,
                        request_id = %request_id,
                        "Grok shim failed to interrupt request after disconnect"
                    );
                }
            }
        }
        Ok(())
    }

    /// Drain one pending entry: resolve its deferred response with the given
    /// stop reason and return the submitted request id (if any) so the caller
    /// can interrupt it. A cancel that fires before the request id is known
    /// still latches `cancel_before_id`, which the submitter observes.
    async fn drain_entry(
        &self,
        key: &(String, String),
        stop_reason: StopReason,
    ) -> Option<String> {
        let entry = self.pending.lock().await.remove(key);
        let mut entry = entry?;
        let _first_cancel = entry.cancel_before_id.lock().await.cancel();
        entry.resolve(Ok(json!({
            "stopReason": stop_reason.wire_name(),
        })));
        entry.request_id.clone()
    }

    /// Interrupt the submitted request and drain the pending entry.
    async fn interrupt_and_drain(&self, key: &(String, String), request_id: &str) {
        self.drain_entry(key, StopReason::Cancelled).await;
        self.interrupt_submitted(request_id).await;
    }

    async fn remove_pending(&self, key: &(String, String)) {
        let entry = self.pending.lock().await.remove(key);
        if let Some(mut entry) = entry {
            entry.resolve(Err(anyhow::anyhow!("pending prompt removed")));
        }
    }

    async fn interrupt_submitted(&self, request_id: &str) {
        if let Err(error) = gents::interrupt_request(self.node.as_ref(), request_id).await {
            tracing::warn!(
                %error,
                request_id,
                "Grok shim failed to interrupt submitted request"
            );
        }
    }

    /// Interrupt runtime child `AgentRequest` rows linked to the parent by
    /// `caused_by_parent_request_id`. Static `Task` rows are never queried or
    /// mutated as runtime state.
    async fn interrupt_child_requests(&self, parent_request_id: &str) {
        let escaped_parent = escape_graphql_string(parent_request_id);
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        caused_by_parent_request_id: {{ _eq: "{escaped_parent}" }}
                    }}
                ) {{
                    request_id
                    lifecycle_state
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        if let Err(error) = ensure_no_errors(&response, "grok shim child request query") {
            tracing::warn!(
                %error,
                parent_request_id,
                "Grok shim failed to load child requests for cancelSubagents"
            );
            return;
        }
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for row in rows {
            let Some(request_id) = row.get("request_id").and_then(Value::as_str) else {
                continue;
            };
            let lifecycle_state = row
                .get("lifecycle_state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if is_terminal_lifecycle_state(lifecycle_state) {
                continue;
            }
            if let Err(error) = gents::interrupt_request(self.node.as_ref(), request_id).await {
                tracing::warn!(
                    %error,
                    request_id,
                    parent_request_id,
                    "Grok shim failed to interrupt child request after cancelSubagents"
                );
            }
        }
    }

    /// Submit the durable Gents request for the prompt and return its request
    /// id. The caller registers the id on the pending entry before the first
    /// fallible outbound send.
    async fn submit_request(
        &self,
        request: &PromptRequest,
        prompt_id: &str,
    ) -> Result<String> {
        let content = prompt_text(request);
        let mut metadata = json!({
            "promptId": prompt_id,
        });
        if let Some(screen_mode) = request.screen_mode.as_deref() {
            metadata["screenMode"] = json!(screen_mode);
        }
        if request.send_now {
            metadata["sendNow"] = json!(true);
        }
        let options = crate::RequestSubmitOptions {
            metadata: Some(metadata.to_string()),
            ..Default::default()
        };
        let submitted = crate::create_agent_request(
            self.config.graphql.as_ref(),
            self.config.agent_did.as_str(),
            &content,
            Some(request.session_id.as_str()),
            Some(self.config.behavior_id.as_str()),
            options,
        )
        .await?;
        Ok(submitted.request_id)
    }

    /// Watch the durable request until it terminalizes or the pending entry
    /// is drained by cancel/disconnect. The drain resolves `response_rx`
    /// first, so the watch returns `cancelled` without another poll.
    async fn watch_terminal(
        &self,
        key: &(String, String),
        request_id: &str,
        mut response_rx: oneshot::Receiver<Result<Value>>,
    ) -> Result<StopReason> {
        loop {
            // A cancel/disconnect that drained the entry resolves the
            // response before (or between) terminalization polls.
            if let Ok(result) = response_rx.try_recv() {
                // The drain always resolves `cancelled` today; keep the
                // branch total in case future drains resolve other reasons.
                let value = result?;
                let stop_reason = value
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or(StopReason::Cancelled.wire_name());
                return Ok(stop_reason_from_wire(stop_reason));
            }
            if let Some(stop_reason) = self.request_stop_reason(request_id).await? {
                // Terminalized: remove the pending entry. The response value
                // is built by the caller from the returned stop reason.
                self.pending.lock().await.remove(key);
                return Ok(stop_reason);
            }
            tokio::select! {
                _ = tokio::time::sleep(TERMINAL_POLL_INTERVAL) => {}
                result = &mut response_rx => {
                    if let Ok(result) = result {
                        let value = result?;
                        let stop_reason = value
                            .get("stopReason")
                            .and_then(Value::as_str)
                            .unwrap_or(StopReason::Cancelled.wire_name());
                        return Ok(stop_reason_from_wire(stop_reason));
                    }
                    // Sender dropped without resolving: treat as cancelled.
                    return Ok(StopReason::Cancelled);
                }
            }
        }
    }

    /// Query the durable request's terminal state and project a `stopReason`.
    /// Returns `None` while the request is still non-terminal.
    async fn request_stop_reason(&self, request_id: &str) -> Result<Option<StopReason>> {
        let escaped_request_id = escape_graphql_string(request_id);
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    order: {{ created_at: DESC }},
                    limit: 1
                ) {{
                    request_id
                    lifecycle_state
                    interrupt_requested_at
                }}
                AgentResponse(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    order: {{ created_at: DESC }},
                    limit: 1
                ) {{
                    request_id
                    status
                    interrupted_at
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "grok shim turn terminal query")?;
        let request_row = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned()
            .unwrap_or(Value::Null);
        if request_row.is_null() {
            // The request row has not been durably observed yet.
            return Ok(None);
        }
        let response_row = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentResponse"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned()
            .unwrap_or(Value::Null);
        let lifecycle_state = request_row
            .get("lifecycle_state")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response_status = response_row
            .get("status")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let interrupted_at = response_row
            .get("interrupted_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        Ok(stop_reason_from_rows(
            lifecycle_state,
            response_status.as_deref(),
            interrupted_at.as_deref(),
        ))
    }
}

/// Map a wire `stopReason` back onto the enum (used by the drain branch).
fn stop_reason_from_wire(name: &str) -> StopReason {
    match name {
        "end_turn" => StopReason::EndTurn,
        "refusal" => StopReason::Refusal,
        "error" => StopReason::Error,
        _ => StopReason::Cancelled,
    }
}

/// Whether a durable request lifecycle state is terminal.
pub(super) fn is_terminal_lifecycle_state(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "superseded" | "dead" | "interrupted"
    )
}

/// Project the durable terminal state into a wire `stopReason`.
///
/// The durable source is `AgentRequest.lifecycle_state`; an `interrupted`
/// request projects `cancelled`. `AgentResponse.interrupted_at` is the durable
/// cancellation marker, so a non-terminal request carrying a non-empty
/// `interrupted_at` is already on its way to `interrupted` and projects
/// `cancelled` promptly.
pub(super) fn stop_reason_from_rows(
    lifecycle_state: &str,
    response_status: Option<&str>,
    interrupted_at: Option<&str>,
) -> Option<StopReason> {
    let interrupted_at_nonempty = interrupted_at
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    match lifecycle_state {
        "interrupted" => Some(StopReason::Cancelled),
        "completed" => match response_status {
            Some("refusal") => Some(StopReason::Refusal),
            Some("error") => Some(StopReason::Error),
            _ => Some(StopReason::EndTurn),
        },
        "failed" => Some(StopReason::Error),
        "superseded" | "dead" => {
            if interrupted_at_nonempty {
                Some(StopReason::Cancelled)
            } else {
                Some(StopReason::Error)
            }
        }
        _ => {
            if interrupted_at_nonempty {
                Some(StopReason::Cancelled)
            } else {
                None
            }
        }
    }
}

/// Flatten the prompt blocks into the single text content submitted to the
/// Gents runtime. Text blocks are joined with newlines; non-text blocks are
/// serialized so their payload is preserved verbatim in the request content.
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

/// Parse the audited `session/prompt` params.
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

/// Parse the audited `session/cancel` notification params.
pub(super) fn parse_cancel_notification(params: &Value) -> Result<CancelNotification> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("session/cancel requires sessionId")?
        .to_string();
    let meta = params.get("_meta");
    let cancel_subagents = meta
        .and_then(|meta| meta.get("cancelSubagents"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
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
        cancel_subagents,
        cancel_trigger,
        rewind_if_no_output,
        rewind_if_pristine,
        prompt_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_block(text: &str) -> Value {
        json!({"type": "text", "text": text})
    }

    #[test]
    fn parse_prompt_request_reads_audited_meta() {
        let params = json!({
            "sessionId": "session-1",
            "prompt": [
                {
                    "type": "text",
                    "text": "hello",
                    "meta": {"skillTokenRanges": [[0, 5]]},
                },
            ],
            "_meta": {
                "promptId": "prompt-1",
                "screenMode": "fullscreen",
                "sendNow": true,
            },
        });
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
        let params = json!({
            "sessionId": "session-1",
            "prompt": [text_block("hi")],
        });
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
        let params = json!({
            "sessionId": "session-1",
            "_meta": {
                "cancelSubagents": true,
                "cancelTrigger": "user",
                "rewindIfNoOutput": true,
                "rewindIfPristine": true,
                "promptId": "prompt-1",
            },
        });
        let notification = parse_cancel_notification(&params).unwrap();
        assert_eq!(notification.session_id, "session-1");
        assert!(notification.cancel_subagents);
        assert_eq!(notification.cancel_trigger.as_deref(), Some("user"));
        assert!(notification.rewind_if_no_output);
        assert!(notification.rewind_if_pristine);
        assert_eq!(notification.prompt_id.as_deref(), Some("prompt-1"));
        let meta = notification.meta();
        assert_eq!(meta["cancelSubagents"], json!(true));
        assert_eq!(meta["cancelTrigger"], json!("user"));
        assert_eq!(meta["rewindIfNoOutput"], json!(true));
        assert_eq!(meta["rewindIfPristine"], json!(true));
        assert_eq!(meta["promptId"], json!("prompt-1"));
    }

    #[test]
    fn parse_cancel_notification_omits_absent_optional_keys() {
        let params = json!({"sessionId": "session-1", "_meta": {"cancelSubagents": false}});
        let notification = parse_cancel_notification(&params).unwrap();
        assert!(!notification.cancel_subagents);
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
            stop_reason_from_rows("interrupted", Some("complete"), None),
            Some(StopReason::Cancelled)
        );
        assert_eq!(
            stop_reason_from_rows("completed", Some("complete"), None),
            Some(StopReason::EndTurn)
        );
        assert_eq!(
            stop_reason_from_rows("completed", Some("refusal"), None),
            Some(StopReason::Refusal)
        );
        assert_eq!(
            stop_reason_from_rows("completed", Some("error"), None),
            Some(StopReason::Error)
        );
        assert_eq!(
            stop_reason_from_rows("failed", Some("error"), None),
            Some(StopReason::Error)
        );
        assert_eq!(stop_reason_from_rows("processing", None, None), None);
    }

    #[test]
    fn stop_reason_projection_maps_interrupted_at_marker() {
        assert_eq!(
            stop_reason_from_rows("processing", None, Some("2026-01-01T00:00:00Z")),
            Some(StopReason::Cancelled)
        );
        assert_eq!(stop_reason_from_rows("processing", None, Some("  ")), None);
        assert_eq!(
            stop_reason_from_rows("superseded", None, Some("2026-01-01T00:00:00Z")),
            Some(StopReason::Cancelled)
        );
        assert_eq!(stop_reason_from_rows("superseded", None, None), Some(StopReason::Error));
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

    #[test]
    fn bash_block_meta_stamp_is_recognized() {
        let block = PromptBlock {
            kind: "text".to_string(),
            text: "$ run".to_string(),
            meta: Some(json!({"bash": {"command": "echo hi"}})),
        };
        assert_eq!(block.bash_command().as_deref(), Some("echo hi"));
        assert!(!block.has_skill_token_ranges());
    }

    #[test]
    fn terminal_lifecycle_states_are_audited() {
        for state in ["completed", "failed", "superseded", "dead", "interrupted"] {
            assert!(is_terminal_lifecycle_state(state), "{state} must be terminal");
        }
        for state in ["pending", "processing", "queued", ""] {
            assert!(!is_terminal_lifecycle_state(state), "{state} must not be terminal");
        }
    }

    /// The cancel-before-id latch records the first cancel and stays set.
    #[test]
    fn cancel_before_id_latch_is_idempotent() {
        let mut latch = CancelBeforeIdLatch::default();
        assert!(!latch.is_cancelled());
        assert!(latch.cancel());
        assert!(latch.is_cancelled());
        assert!(!latch.cancel());
        assert!(latch.is_cancelled());
    }

    /// A pending entry resolves its deferred response exactly once.
    #[tokio::test]
    async fn pending_prompt_resolves_once() {
        let (tx, mut rx) = oneshot::channel::<Result<Value>>();
        let mut entry = PendingPrompt {
            response_tx: Some(tx),
            request_id: None,
            cancel_before_id: Arc::new(Mutex::new(CancelBeforeIdLatch::default())),
            drained: false,
        };
        entry.resolve(Ok(json!({"stopReason": "cancelled"})));
        assert!(entry.drained);
        let first = rx.try_recv().expect("first resolve delivers");
        assert_eq!(first.unwrap()["stopReason"], json!("cancelled"));
        // A second resolve is a no-op: the receiver is exhausted.
        entry.resolve(Ok(json!({"stopReason": "end_turn"})));
        assert!(rx.try_recv().is_err());
    }

    /// The user echo notification carries the audited shape: content field
    /// name (not contentBlock), promptIndex/hideFromScrollback block meta,
    /// and _meta.promptId.
    #[tokio::test]
    async fn user_echo_uses_audited_chunk_shape() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sender = PromptSender::Buffer {
            buffer: buffer.clone(),
        };
        sender
            .send_user_message_chunk(
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
        // The Grok decoder expects the chunk field name `content`.
        assert_eq!(value["params"]["update"]["content"]["type"], json!("text"));
        assert_eq!(value["params"]["update"]["content"]["text"], json!("hello"));
        assert_eq!(
            value["params"]["update"]["content"]["meta"]["promptIndex"],
            json!(0)
        );
        assert_eq!(
            value["params"]["update"]["content"]["meta"]["hideFromScrollback"],
            json!(false)
        );
        assert_eq!(value["params"]["_meta"]["promptId"], json!("prompt-1"));
        assert_eq!(value["params"]["_meta"]["isReplay"], json!(false));
    }

    /// A closed outbound channel makes the user echo send fail, which is the
    /// send-failure-after-submission path: the caller must interrupt.
    #[tokio::test]
    async fn closed_outbound_channel_fails_the_echo_send() {
        let (outbound_tx, _outbound_rx) = mpsc::unbounded_channel::<String>();
        drop(_outbound_rx);
        let sender = PromptSender::Line {
            connection_id: 1,
            outbound_tx,
        };
        let result = sender
            .send_user_message_chunk(
                "session-1",
                "prompt-1",
                &PromptBlock {
                    kind: "text".to_string(),
                    text: "hello".to_string(),
                    meta: None,
                },
                0,
            )
            .await;
        assert!(result.is_err(), "closed channel must fail the send");
    }

    /// The wire-escape helper is applied to every interpolated GraphQL value:
    /// a request id containing quotes and backslashes must round-trip safely
    /// through the terminal query string.
    #[test]
    fn terminal_query_escapes_interpolated_values() {
        let request_id = "req-\"quoted\"\\slash";
        let escaped = escape_graphql_string(request_id);
        let query = format!(
            r#"AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }})"#
        );
        assert!(!query.contains(r#""req-""#), "raw quote must be escaped");
        assert!(query.contains(&escaped));
    }

    /// One pending prompt per session: the live-prompt check treats any
    /// pending entry for the same session as a conflict.
    #[tokio::test]
    async fn one_pending_prompt_per_session_is_enforced_by_key_scan() {
        let pending = Mutex::new(HashMap::new());
        let (tx, _rx) = oneshot::channel::<Result<Value>>();
        pending.lock().await.insert(
            ("session-1".to_string(), "prompt-live".to_string()),
            PendingPrompt {
                response_tx: Some(tx),
                request_id: None,
                cancel_before_id: Arc::new(Mutex::new(CancelBeforeIdLatch::default())),
                drained: false,
            },
        );
        let has_live = pending
            .lock()
            .await
            .keys()
            .any(|(session, _)| session == "session-1");
        assert!(has_live);
        let other_session_live = pending
            .lock()
            .await
            .keys()
            .any(|(session, _)| session == "session-2");
        assert!(!other_session_live);
    }

    // ----- Integration tests: real embedded node + mock GraphQL endpoint -----

    use axum::{extract::State, routing::post, Json, Router};

    /// Shared mock-endpoint state: the embedded node plus an optional
    /// one-shot submission gate. While armed, the endpoint signals that the
    /// `create_AgentRequest` mutation has arrived and then parks until the
    /// test releases it, so a cancel/disconnect can deterministically land
    /// inside the before-request-id window. The gate disarms itself after
    /// the first gated submission so later submissions pass through.
    #[derive(Clone)]
    struct MockGraphqlState {
        node: Arc<EmbeddedNode>,
        gate_armed: Option<Arc<std::sync::atomic::AtomicBool>>,
        submission_arrived: Option<tokio::sync::Notify>,
        submission_release: Option<Arc<tokio::sync::Notify>>,
    }

    /// A mock GraphQL endpoint that forwards mutations to the embedded node
    /// so `create_agent_request` writes real durable rows. Every response is
    /// the node's own, so the whole submission path is exercised.
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
        if is_create_request {
            let first_gated_submission = state
                .gate_armed
                .as_ref()
                .is_some_and(|armed| {
                    !armed.swap(false, std::sync::atomic::Ordering::SeqCst)
                });
            if first_gated_submission {
                if let Some(arrived) = state.submission_arrived.as_ref() {
                    // notify_one stores a permit when no test waiter is
                    // parked yet, so the arrival signal cannot be lost.
                    arrived.notify_one();
                }
                if let Some(release) = state.submission_release.as_ref() {
                    release.notified().await;
                }
            }
        }
        let response = state.node.execute(&query).await;
        Json(serde_json::to_value(&response).unwrap_or_default())
    }

    /// Spawn a mock GraphQL endpoint bound to the node and return its URL.
    /// The endpoint forwards to the node directly; no gating is applied.
    async fn spawn_mock_graphql(node: Arc<EmbeddedNode>) -> String {
        spawn_gated_mock_graphql(node, None).await
    }

    /// Spawn a mock GraphQL endpoint that one-shot gates the first
    /// `create_AgentRequest`: the returned `Notify` fires when the submission
    /// mutation arrives, and the endpoint parks the mutation until
    /// `submission_release` is notified. Later submissions pass through.
    async fn spawn_gated_mock_graphql(
        node: Arc<EmbeddedNode>,
        submission_gate: Option<(
            tokio::sync::Notify,
            Arc<tokio::sync::Notify>,
            Arc<std::sync::atomic::AtomicBool>,
        )>,
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

    /// Build an embedded node with runtime schemas, matching the crate's
    /// `persistent_node_builder` convention (Lark backend).
    async fn test_node() -> (tempfile::TempDir, Arc<EmbeddedNode>) {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(tempdir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Lark)
                .build()
                .await
                .expect("embedded node"),
        );
        gents::schema::ensure_runtime_schemas(&node)
            .await
            .expect("runtime schemas");
        (tempdir, node)
    }

    fn test_config(graphql: String) -> TurnManagerConfig {
        TurnManagerConfig {
            agent_did: "did:test:grok-shim".to_string(),
            behavior_id: "did:test:grok-shim:default".to_string(),
            graphql,
        }
    }

    fn buffer_sender() -> (Arc<Mutex<Vec<String>>>, PromptSender) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        (
            buffer.clone(),
            PromptSender::Buffer {
                buffer,
            },
        )
    }

    /// Terminalize a request durably in one atomic mutation: the given
    /// lifecycle state plus a response row carrying the audited fields, as
    /// the runtime does. A single mutation avoids the watch observing a
    /// transient intermediate state between two writes.
    async fn terminalize_request(node: &Arc<EmbeddedNode>, request_id: &str, lifecycle_state: &str) {
        let escaped = escape_graphql_string(request_id);
        let escaped_state = escape_graphql_string(lifecycle_state);
        let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                    input: {{ lifecycle_state: "{escaped_state}" }}
                ) {{ _docID }}
                create_AgentResponse(input: {{
                    response_key: "{escaped}"
                    request_id: "{escaped}"
                    agent_did: "did:test:grok-shim"
                    behavior_id: "did:test:grok-shim:default"
                    session_id: "session-1"
                    content: ""
                    reasoning: ""
                    status: "complete"
                    error_message: ""
                    token_count: 0
                    progress_seq: 0
                    created_at: "{now}"
                    completed_at: "{now}"
                }}) {{ _docID }}
            }}"#
        );
        let response = node.execute(&mutation).await;
        ensure_no_errors(&response, "test terminalize request").expect("terminalize");
    }

    /// A prompt that terminalizes normally resolves `stopReason=end_turn`.
    #[tokio::test]
    async fn prompt_resolves_end_turn_after_terminalization() {
        let (_tempdir, node) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = TurnManager::new(node.clone(), test_config(graphql));
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
        let handle = tokio::spawn(async move {
            // Wait for the request row to exist, then terminalize it.
            loop {
                let query = r#"{ AgentRequest(filter: { lifecycle_state: { _eq: "pending" } }) { request_id } }"#;
                let response = node_for_terminalize.execute(query).await;
                let rows = response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if let Some(row) = rows.first() {
                    let request_id = row.get("request_id").and_then(Value::as_str).unwrap();
                    // A completed lifecycle with a `complete` response status
                    // projects `end_turn`; the single atomic mutation avoids
                    // the watch observing a transient `interrupted` state.
                    terminalize_request(&node_for_terminalize, request_id, "completed").await;
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });

        let result = tokio::time::timeout(Duration::from_secs(30), manager.handle_prompt(prompt, &sender))
            .await
            .expect("prompt should resolve within timeout")
            .expect("prompt should succeed");
        assert_eq!(result["stopReason"], json!("end_turn"));
        handle.await.expect("terminalize task");
    }

    /// Cancel before the request id is registered: the entry is drained, the
    /// connected prompt resolves `stopReason=cancelled`, and the next prompt
    /// for the session is accepted (reuse). The gated mock endpoint holds the
    /// submitter inside `create_agent_request`, so the cancel deterministically
    /// lands in the before-request-id window.
    #[tokio::test]
    async fn cancel_before_request_id_resolves_cancelled_and_permits_reuse() {
        let (_tempdir, node) = test_node().await;
        let submission_arrived = tokio::sync::Notify::new();
        let submission_release = Arc::new(tokio::sync::Notify::new());
        let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let graphql = spawn_gated_mock_graphql(
            node.clone(),
            Some((submission_arrived, submission_release.clone(), gate_armed)),
        )
        .await;
        let manager = Arc::new(TurnManager::new(node.clone(), test_config(graphql)));
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

        // Run the prompt; it parks inside the gated submission with its
        // pending entry inserted and no request id registered yet.
        let prompt_handle = tokio::spawn({
            let manager = manager.clone();
            let sender = sender.clone();
            async move { manager.handle_prompt(prompt, &sender).await }
        });

        // Deterministically wait for the submission mutation to arrive: the
        // submitter is now inside create_agent_request, strictly before the
        // request id is registered.
        tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
            .await
            .expect("submission should arrive at the gated endpoint");

        // Cancel while the submitter is parked: this drains the pending entry
        // and latches cancel-before-id.
        let cancel = parse_cancel_notification(&json!({
            "sessionId": "session-1",
            "_meta": {"cancelSubagents": true, "promptId": "prompt-1"},
        }))
        .unwrap();
        manager
            .handle_cancel(cancel)
            .await
            .expect("cancel should succeed");

        // Release the submission; the submitter observes the latch and
        // resolves the prompt with `stopReason=cancelled`.
        submission_release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
            .await
            .expect("prompt should resolve within timeout")
            .expect("prompt task")
            .expect("cancel-before-id must resolve cancelled, not error");

        assert_eq!(result["stopReason"], json!("cancelled"));

        // Reuse: the session accepts the next prompt. Run the second prompt
        // and a terminalizer concurrently: any request still pending (the
        // orphaned first submission and/or the second prompt's fresh one) is
        // terminalized as interrupted so the second prompt resolves.
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
        let terminalize_handle = tokio::spawn(async move {
            loop {
                let query = r#"{ AgentRequest(filter: { lifecycle_state: { _eq: "pending" } }) { request_id } }"#;
                let response = node_for_terminalize.execute(query).await;
                let rows = response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for row in rows {
                    let request_id = row.get("request_id").and_then(Value::as_str).unwrap();
                    terminalize_request(&node_for_terminalize, request_id, "interrupted").await;
                }
                if rows.is_empty() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        });
        let result = tokio::time::timeout(Duration::from_secs(30), manager.handle_prompt(second, &sender))
            .await
            .expect("second prompt should resolve within timeout")
            .expect("second prompt should succeed");
        assert_eq!(result["stopReason"], json!("cancelled"));
        terminalize_handle.abort();
    }

    /// Disconnect before the request id is registered: the entry is drained
    /// and the prompt resolves `stopReason=cancelled`. The gated mock endpoint
    /// holds the submitter inside `create_agent_request`, so the disconnect
    /// deterministically lands in the before-request-id window.
    #[tokio::test]
    async fn disconnect_before_request_id_resolves_cancelled() {
        let (_tempdir, node) = test_node().await;
        let submission_arrived = tokio::sync::Notify::new();
        let submission_release = Arc::new(tokio::sync::Notify::new());
        let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let graphql = spawn_gated_mock_graphql(
            node.clone(),
            Some((submission_arrived, submission_release.clone(), gate_armed)),
        )
        .await;
        let manager = Arc::new(TurnManager::new(node.clone(), test_config(graphql)));
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

        // Run the prompt; it parks inside the gated submission with its
        // pending entry inserted and no request id registered yet.
        let prompt_handle = tokio::spawn({
            let manager = manager.clone();
            let sender = sender.clone();
            async move { manager.handle_prompt(prompt, &sender).await }
        });

        // Deterministically wait for the submission mutation to arrive.
        tokio::time::timeout(Duration::from_secs(30), submission_arrived.notified())
            .await
            .expect("submission should arrive at the gated endpoint");

        // Disconnect while the submitter is parked: this drains the pending
        // entry and latches cancel-before-id for the parked submitter.
        manager
            .handle_disconnect()
            .await
            .expect("disconnect should succeed");

        // Release the submission; the submitter observes the latch and
        // resolves the prompt with `stopReason=cancelled`.
        submission_release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(30), prompt_handle)
            .await
            .expect("prompt should resolve within timeout")
            .expect("prompt task")
            .expect("disconnect-before-id must resolve cancelled, not error");
        assert_eq!(result["stopReason"], json!("cancelled"));
    }

    /// Send failure after submission: the user echo fails against a closed
    /// outbound channel, so the submitted request must be interrupted and the
    /// prompt surface the send failure.
    #[tokio::test]
    async fn send_failure_after_submission_interrupts_the_request() {
        let (_tempdir, node) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = TurnManager::new(node.clone(), test_config(graphql));
        let (outbound_tx, _outbound_rx) = mpsc::unbounded_channel::<String>();
        drop(_outbound_rx);
        let sender = PromptSender::Line {
            connection_id: 1,
            outbound_tx,
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

        let result = tokio::time::timeout(Duration::from_secs(30), manager.handle_prompt(prompt, &sender))
            .await
            .expect("prompt should resolve within timeout");
        assert!(result.is_err(), "closed outbound must surface a send failure");

        // The submitted request must have been interrupted: its durable row
        // carries a non-empty interrupt_requested_at.
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

    /// A second prompt for the same session while one is live is rejected and
    /// does not disturb the live turn (one pending prompt per session).
    #[tokio::test]
    async fn second_prompt_for_live_session_is_rejected() {
        let (_tempdir, node) = test_node().await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let manager = Arc::new(TurnManager::new(node.clone(), test_config(graphql)));
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
        // Run the first prompt to its terminal watch; nothing terminalizes it
        // yet, so it stays pending for the whole rejection check below.
        let manager_for_first = manager.clone();
        let sender_for_first = sender.clone();
        let first_handle = tokio::spawn(async move {
            manager_for_first.handle_prompt(first, &sender_for_first).await
        });

        // The first prompt's user echo is sent only after its request id was
        // registered on the pending entry, so a non-empty buffer proves the
        // entry is live.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if buffer.lock().await.len() >= 1 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "first prompt never echoed; pending entry never went live"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // A second prompt for the same session must be rejected while the
        // first is still pending.
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
            .handle_prompt(second, &sender)
            .await
            .expect_err("second prompt for a live session must be rejected");
        assert!(
            rejection.to_string().contains("live prompt"),
            "rejection must name the one-pending-per-session rule, got: {rejection}"
        );

        // The rejection must not have disturbed the live turn: terminalize the
        // first prompt's request and confirm it resolves normally.
        let node_for_terminalize = node.clone();
        let terminalize_handle = tokio::spawn(async move {
            loop {
                let query = r#"{ AgentRequest(filter: { lifecycle_state: { _eq: "pending" } }) { request_id } }"#;
                let response = node_for_terminalize.execute(query).await;
                let rows = response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if let Some(row) = rows.first() {
                    let request_id = row.get("request_id").and_then(Value::as_str).unwrap();
                    terminalize_request(&node_for_terminalize, request_id, "completed").await;
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
