use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::llm::tool::ToolDyn;
use crate::llm::{HookAction, ToolCallHookAction};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::background_tools::LiveToolOutputRegistry;
use crate::session;
use crate::tool_call_lifecycle::{
    AwaitMode, CancelCause, CascadeDispatch, ChildTerminal, ToolCallLifecycle,
};
use crate::truncation::TruncationLimits;

pub(crate) mod persistence;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailurePolicy {
    FailOpen,
    #[default]
    FailClosed,
}

#[derive(Debug)]
pub struct HookStats {
    pub persistence_failures: u64,
    pub persistence_successes: u64,
}

struct HookCounters {
    failures: AtomicU64,
    successes: AtomicU64,
}

#[derive(Clone, Default)]
pub struct BackgroundToolRegistry {
    inner: Arc<BackgroundToolRegistryInner>,
}

#[derive(Default)]
struct BackgroundToolRegistryInner {
    tools: HashMap<String, Arc<Mutex<Box<dyn ToolDyn>>>>,
    allowlist: Vec<String>,
}

impl BackgroundToolRegistry {
    pub fn from_tools(tools: Vec<Box<dyn ToolDyn>>, allowlist: &[String]) -> Self {
        let allowed = allowlist
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect::<HashSet<_>>();
        let mut registry_tools = HashMap::new();
        for tool in tools {
            let name = tool.name();
            if allowed.contains(&name) {
                registry_tools.insert(name, Arc::new(Mutex::new(tool)));
            }
        }
        let mut allowlist = allowed.into_iter().collect::<Vec<_>>();
        allowlist.sort();
        Self {
            inner: Arc::new(BackgroundToolRegistryInner {
                tools: registry_tools,
                allowlist,
            }),
        }
    }

    pub(crate) fn get(&self, tool_name: &str) -> Option<Arc<Mutex<Box<dyn ToolDyn>>>> {
        self.inner.tools.get(tool_name).cloned()
    }

    pub(crate) fn allowlist(&self) -> Vec<String> {
        self.inner.allowlist.clone()
    }
}

#[derive(Clone)]
struct BackgroundExecution {
    cancellation_token: CancellationToken,
}

#[derive(Clone, Default)]
pub struct BackgroundExecutionRegistry {
    inner: Arc<Mutex<HashMap<String, BackgroundExecution>>>,
}

impl BackgroundExecutionRegistry {
    pub async fn cancel(&self, tool_call_id: &str) -> bool {
        let Some(execution) = self.inner.lock().await.get(tool_call_id).cloned() else {
            return false;
        };
        execution.cancellation_token.cancel();
        true
    }

    pub(crate) async fn insert(&self, tool_call_id: String, cancellation_token: CancellationToken) {
        self.inner
            .lock()
            .await
            .insert(tool_call_id, BackgroundExecution { cancellation_token });
    }

    pub(crate) async fn remove(&self, tool_call_id: &str) {
        self.inner.lock().await.remove(tool_call_id);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptTurnState {
    Idle,
    AssistantBuilding { sequence: u32 },
    AssistantPersisted { sequence: u32 },
}

#[derive(Debug, Clone, Default)]
struct ToolResultIdentity {
    result_id: Option<String>,
    call_id: Option<String>,
}

struct SessionState {
    session_id: Option<String>,
    current_request_id: Option<String>,
    current_requester_did: Option<String>,
    request_deadline_at: Option<DateTime<Utc>>,
    approval_required_tools: Vec<String>,
    agent_name: String,
    sequence: u32,
    transcript_turn: TranscriptTurnState,
    persisted_tool_result_keys: HashSet<String>,
    persisted_tool_result_message_sequences: HashMap<String, u32>,
    tool_result_identities: HashMap<String, ToolResultIdentity>,
    initialized: bool,
}

impl SessionState {
    /// Reset on a genuine user message only. Tool-result messages must NOT
    /// reset the turn state: with several parallel tool calls accumulated in
    /// one persisted assistant turn, the first streamed result's user message
    /// would otherwise revoke the persisted-turn gate the remaining results
    /// still need (Lean: `Transcript.parallel_results_complete_independently`;
    /// `completeToolWithResult` never removes a persisted reservation).
    fn reset_after_user_message(&mut self) {
        self.transcript_turn = TranscriptTurnState::Idle;
    }

    fn begin_or_continue_assistant_turn(&mut self) -> u32 {
        match self.transcript_turn {
            TranscriptTurnState::AssistantBuilding { sequence } => sequence,
            TranscriptTurnState::Idle | TranscriptTurnState::AssistantPersisted { .. } => {
                self.sequence += 1;
                let sequence = self.sequence;
                self.transcript_turn = TranscriptTurnState::AssistantBuilding { sequence };
                sequence
            }
        }
    }

    fn persist_assistant_turn(&mut self) -> u32 {
        let sequence = match self.transcript_turn {
            TranscriptTurnState::AssistantBuilding { sequence } => sequence,
            // `AssistantPersisted` means the PREVIOUS turn closed (tool-result
            // messages keep it, see `reset_after_user_message`); persisting an
            // assistant message from here starts a new turn, exactly like Idle
            // (e.g. a text-only final turn after tool results).
            TranscriptTurnState::Idle | TranscriptTurnState::AssistantPersisted { .. } => {
                self.sequence += 1;
                self.sequence
            }
        };
        self.transcript_turn = TranscriptTurnState::AssistantPersisted { sequence };
        sequence
    }

    fn register_tool_result_identity(
        &mut self,
        internal_call_id: &str,
        result_id: Option<&str>,
        call_id: Option<&str>,
    ) {
        let identity = self
            .tool_result_identities
            .entry(internal_call_id.to_string())
            .or_default();
        if let Some(result_id) = non_empty(result_id) {
            identity.result_id = Some(result_id.to_string());
        }
        if let Some(call_id) = non_empty(call_id) {
            identity.call_id = Some(call_id.to_string());
        }
    }

    fn tool_result_message_identity(
        &self,
        internal_call_id: &str,
        call_id: Option<&str>,
    ) -> (String, Option<String>) {
        let registered = self.tool_result_identities.get(internal_call_id);
        let result_id = registered
            .and_then(|identity| identity.result_id.clone())
            .or_else(|| non_empty(call_id).map(ToOwned::to_owned))
            .unwrap_or_else(|| internal_call_id.to_string());
        let call_id = registered
            .and_then(|identity| identity.call_id.clone())
            .or_else(|| non_empty(call_id).map(ToOwned::to_owned));

        (result_id, call_id)
    }

    fn mark_tool_result_seen_for_persisted_turn(
        &mut self,
        internal_call_id: &str,
        result_id: Option<&str>,
        call_id: Option<&str>,
    ) -> bool {
        self.register_tool_result_identity(internal_call_id, result_id, call_id);
        if !matches!(
            self.transcript_turn,
            TranscriptTurnState::AssistantPersisted { .. }
        ) {
            return false;
        }

        let keys = self.tool_result_dedupe_keys(internal_call_id, result_id, call_id);
        self.mark_tool_result_keys_seen(keys)
    }

    fn mark_stream_tool_result_seen(
        &mut self,
        internal_call_id: &str,
        result_id: &str,
        call_id: Option<&str>,
    ) -> anyhow::Result<bool> {
        self.register_tool_result_identity(internal_call_id, Some(result_id), call_id);
        let keys = self.tool_result_dedupe_keys(internal_call_id, Some(result_id), call_id);
        if !matches!(
            self.transcript_turn,
            TranscriptTurnState::AssistantPersisted { .. }
        ) {
            if self.tool_result_keys_already_seen(&keys) {
                self.persist_tool_result_keys(keys);
                return Ok(false);
            }
            anyhow::bail!(
                "cannot persist streamed tool result before its assistant turn is persisted"
            );
        }
        Ok(self.mark_tool_result_keys_seen(keys))
    }

    /// True once the current turn's assistant message has been persisted
    /// (`TranscriptTurnState::AssistantPersisted`). Tool-result messages may
    /// only be persisted after this gate, so the abort-path backfill checks it
    /// before reconciling completed-but-unmessaged tool calls (#442).
    fn assistant_turn_persisted(&self) -> bool {
        matches!(
            self.transcript_turn,
            TranscriptTurnState::AssistantPersisted { .. }
        )
    }

    fn tool_result_dedupe_keys(
        &self,
        internal_call_id: &str,
        result_id: Option<&str>,
        call_id: Option<&str>,
    ) -> Vec<String> {
        // The hook, stream item, and transcript can expose different IDs for
        // the same tool result. Persist all known aliases and skip when any
        // alias has already materialized the result message.
        let mut keys = Vec::new();
        push_tool_result_key(&mut keys, "internal", Some(internal_call_id));

        if let Some(identity) = self.tool_result_identities.get(internal_call_id) {
            push_tool_result_key(&mut keys, "result", identity.result_id.as_deref());
            push_tool_result_key(&mut keys, "call", identity.call_id.as_deref());
        }
        push_tool_result_key(&mut keys, "result", result_id);
        push_tool_result_key(&mut keys, "call", call_id);

        keys
    }

    fn mark_tool_result_keys_seen(&mut self, keys: Vec<String>) -> bool {
        let already_seen = self.tool_result_keys_already_seen(&keys);
        self.persist_tool_result_keys(keys);
        !already_seen
    }

    fn tool_result_keys_already_seen(&self, keys: &[String]) -> bool {
        keys.iter()
            .any(|key| self.persisted_tool_result_keys.contains(key))
    }

    fn persist_tool_result_keys(&mut self, keys: Vec<String>) {
        self.persisted_tool_result_keys.extend(keys);
    }
}

fn push_tool_result_key(keys: &mut Vec<String>, namespace: &str, value: Option<&str>) {
    let Some(value) = non_empty(value) else {
        return;
    };
    let key = format!("{namespace}:{value}");
    if !keys.iter().any(|existing| existing == &key) {
        keys.push(key);
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| (!value.is_empty()).then_some(value))
}

#[derive(Clone)]
pub struct DefraSessionHook {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    truncation_limits: TruncationLimits,
    failure_policy: FailurePolicy,
    counters: Arc<HookCounters>,
    state: Arc<Mutex<SessionState>>,
    in_flight_lifecycles: Arc<Mutex<HashMap<String, ToolCallLifecycle>>>,
    background_tool_registry: BackgroundToolRegistry,
    background_executions: BackgroundExecutionRegistry,
    background_live_outputs: LiveToolOutputRegistry,
}

enum PolicyDecision {
    Continue,
    Terminate(String),
}

impl DefraSessionHook {
    pub fn with_identity(
        node: Arc<EmbeddedNode>,
        agent_name: &str,
        agent_did: &str,
        failure_policy: FailurePolicy,
    ) -> Self {
        Self {
            node,
            agent_did: agent_did.to_string(),
            truncation_limits: TruncationLimits::default(),
            failure_policy,
            counters: Arc::new(HookCounters {
                failures: AtomicU64::new(0),
                successes: AtomicU64::new(0),
            }),
            state: Arc::new(Mutex::new(SessionState {
                session_id: None,
                current_request_id: None,
                current_requester_did: None,
                request_deadline_at: None,
                approval_required_tools: Vec::new(),
                agent_name: agent_name.to_string(),
                sequence: 0,
                transcript_turn: TranscriptTurnState::Idle,
                persisted_tool_result_keys: HashSet::new(),
                persisted_tool_result_message_sequences: HashMap::new(),
                tool_result_identities: HashMap::new(),
                initialized: false,
            })),
            in_flight_lifecycles: Arc::new(Mutex::new(HashMap::new())),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_executions: BackgroundExecutionRegistry::default(),
            background_live_outputs: LiveToolOutputRegistry::default(),
        }
    }

    pub async fn resume_with_identity_policy(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        agent_name: &str,
        agent_did: &str,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        session::ensure_session(&node, session_id, agent_name, agent_did).await?;
        let max_seq = session::max_sequence(&node, session_id).await?;

        Ok(Self {
            node,
            agent_did: agent_did.to_string(),
            truncation_limits: TruncationLimits::default(),
            failure_policy,
            counters: Arc::new(HookCounters {
                failures: AtomicU64::new(0),
                successes: AtomicU64::new(0),
            }),
            state: Arc::new(Mutex::new(SessionState {
                session_id: Some(session_id.to_string()),
                current_request_id: None,
                current_requester_did: None,
                request_deadline_at: None,
                approval_required_tools: Vec::new(),
                agent_name: agent_name.to_string(),
                sequence: max_seq,
                transcript_turn: TranscriptTurnState::Idle,
                persisted_tool_result_keys: HashSet::new(),
                persisted_tool_result_message_sequences: HashMap::new(),
                tool_result_identities: HashMap::new(),
                initialized: true,
            })),
            in_flight_lifecycles: Arc::new(Mutex::new(HashMap::new())),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_executions: BackgroundExecutionRegistry::default(),
            background_live_outputs: LiveToolOutputRegistry::default(),
        })
    }

    pub fn with_background_tool_registry(mut self, registry: BackgroundToolRegistry) -> Self {
        self.background_tool_registry = registry;
        self
    }

    pub fn with_background_execution_registry(
        mut self,
        registry: BackgroundExecutionRegistry,
    ) -> Self {
        self.background_executions = registry;
        self
    }

    pub fn stats(&self) -> HookStats {
        HookStats {
            persistence_failures: self.counters.failures.load(Ordering::Relaxed),
            persistence_successes: self.counters.successes.load(Ordering::Relaxed),
        }
    }

    fn record_success(&self) {
        self.counters.successes.fetch_add(1, Ordering::Relaxed);
    }

    fn decide_persistence_outcome(&self, context: &str, error: &anyhow::Error) -> PolicyDecision {
        decide_persistence_outcome(self.failure_policy, &self.counters, context, error)
    }

    fn on_persistence_error(&self, context: &str, error: &anyhow::Error) -> HookAction {
        match self.decide_persistence_outcome(context, error) {
            PolicyDecision::Continue => HookAction::Continue,
            PolicyDecision::Terminate(reason) => HookAction::Terminate { reason },
        }
    }

    fn on_tool_persistence_error(
        &self,
        context: &str,
        error: &anyhow::Error,
    ) -> ToolCallHookAction {
        match self.decide_persistence_outcome(context, error) {
            PolicyDecision::Continue => ToolCallHookAction::Continue,
            PolicyDecision::Terminate(reason) => ToolCallHookAction::Terminate { reason },
        }
    }

    pub async fn resume_or_create_with_identity_policy(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        agent_name: &str,
        agent_did: &str,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        Self::resume_with_identity_policy(node, session_id, agent_name, agent_did, failure_policy)
            .await
    }

    pub async fn session_id(&self) -> Option<String> {
        self.state.lock().await.session_id.clone()
    }

    pub async fn set_active_request_id(&self, request_id: Option<String>) {
        // This compatibility setter intentionally clears requester lineage:
        // carrying a prior coordinator DID across requests would misroute the
        // new request's immutable return artifacts.
        self.set_active_request_lineage(request_id, None).await;
    }

    pub async fn set_active_request_lineage(
        &self,
        request_id: Option<String>,
        requester_did: Option<String>,
    ) {
        let mut state = self.state.lock().await;
        state.current_request_id = request_id;
        state.current_requester_did = requester_did;
    }

    async fn active_requester_did(&self) -> Option<String> {
        self.state.lock().await.current_requester_did.clone()
    }

    pub(crate) async fn register_stream_tool_call_identity(
        &self,
        internal_call_id: &str,
        result_id: &str,
        call_id: Option<&str>,
    ) {
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            Some(result_id),
            call_id,
        );
    }

    pub async fn set_request_deadline_at(&self, deadline_at: Option<DateTime<Utc>>) {
        self.state.lock().await.request_deadline_at = deadline_at;
    }

    /// Set the tool names the behavior policy holds for operator approval.
    pub async fn set_approval_required_tools(&self, tools: Vec<String>) {
        self.state.lock().await.approval_required_tools = tools;
    }

    pub(crate) async fn approval_required_for(&self, tool_name: &str) -> bool {
        self.state
            .lock()
            .await
            .approval_required_tools
            .iter()
            .any(|name| name == tool_name)
    }

    pub(crate) async fn timeout_expired_tool_calls(&self) -> anyhow::Result<usize> {
        let lifecycles = {
            let now = Utc::now();
            let mut map = self.in_flight_lifecycles.lock().await;
            let expired_ids = map
                .iter()
                .filter_map(|(id, lifecycle)| (lifecycle.deadline_at() <= now).then(|| id.clone()))
                .collect::<Vec<_>>();

            expired_ids
                .into_iter()
                .filter_map(|id| map.remove(&id))
                .collect::<Vec<_>>()
        };

        let count = lifecycles.len();
        for mut lifecycle in lifecycles {
            if lifecycle.is_subagent_bridge() {
                if lifecycle.await_mode() == AwaitMode::Foreground {
                    lifecycle.bridge_failure(ChildTerminal::Dead).await?;
                } else {
                    tracing::debug!(
                        "leaving background subagent bridge running after parent deadline sweep"
                    );
                }
            } else if lifecycle.state()
                == crate::tool_call_lifecycle::ToolCallState::AwaitingApproval
            {
                lifecycle.timeout_while_held().await?;
            } else {
                lifecycle.timeout().await?;
            }
        }
        Ok(count)
    }

    pub async fn cancel_in_flight_tool_calls(&self) -> anyhow::Result<usize> {
        let lifecycles = {
            let mut map = self.in_flight_lifecycles.lock().await;
            map.drain()
                .map(|(_, lifecycle)| lifecycle)
                .collect::<Vec<_>>()
        };

        let count = lifecycles.len();
        for mut lifecycle in lifecycles {
            if lifecycle.state() == crate::tool_call_lifecycle::ToolCallState::AwaitingApproval {
                lifecycle
                    .cancel_while_held(CancelCause::Interrupted)
                    .await?;
                continue;
            }
            let dispatch = lifecycle
                .cancel_during_run_with_cascade_dispatch(CancelCause::Interrupted, &self.agent_did)
                .await?;
            if lifecycle.is_cancelled() {
                if let Some(dispatch) = dispatch {
                    if let CascadeDispatch::Local(intent) = dispatch {
                        if let Err(error) = crate::interrupt::interrupt_request(
                            &self.node,
                            &intent.child_request_id,
                        )
                        .await
                        {
                            tracing::warn!(
                                child_request_id = %intent.child_request_id,
                                error = %error,
                                "failed to cascade live tool-call cancellation to child request"
                            );
                        }
                    }
                }
            }
        }
        Ok(count)
    }

    pub(crate) async fn fail_in_flight_tool_calls(
        &self,
        result: &str,
        failure_class: crate::tool_call_lifecycle::FailureClass,
    ) -> anyhow::Result<usize> {
        let lifecycles = {
            let mut map = self.in_flight_lifecycles.lock().await;
            map.drain()
                .map(|(_, lifecycle)| lifecycle)
                .collect::<Vec<_>>()
        };

        let count = lifecycles.len();
        for mut lifecycle in lifecycles {
            if lifecycle.is_subagent_bridge() {
                lifecycle
                    .bridge_failure(ChildTerminal::Failed {
                        reason: result.to_string(),
                        failure_class,
                    })
                    .await?;
            } else {
                lifecycle.fail(result, failure_class).await?;
            }
        }
        Ok(count)
    }

    pub async fn mark_current_response_materialized(&self, sequence: u32) -> anyhow::Result<()> {
        let request_id = self.state.lock().await.current_request_id.clone();
        let Some(request_id) = request_id.as_deref() else {
            return Ok(());
        };
        session::mark_response_materialized(&self.node, request_id, sequence).await
    }

    pub async fn close(&self) -> anyhow::Result<()> {
        let session_id = self.state.lock().await.session_id.clone();
        if let Some(id) = session_id {
            session::close_session(&self.node, &id).await?;
        }
        Ok(())
    }

    pub fn apply_persistence_policy(
        &self,
        result: anyhow::Result<()>,
        context: &str,
    ) -> anyhow::Result<()> {
        match result {
            Ok(()) => {
                self.record_success();
                Ok(())
            }
            Err(e) => match self.decide_persistence_outcome(context, &e) {
                PolicyDecision::Continue => Ok(()),
                PolicyDecision::Terminate(_) => Err(e),
            },
        }
    }
}

impl Drop for DefraSessionHook {
    fn drop(&mut self) {
        // Drain the in-flight map. Lifecycles dropped without completing a
        // transition leave their AgentToolCall row in state Running on disk —
        // startup recovery sweeps these on daemon restart.
        if let Ok(mut map) = self.in_flight_lifecycles.try_lock() {
            map.clear();
        }
    }
}

fn decide_persistence_outcome(
    failure_policy: FailurePolicy,
    counters: &HookCounters,
    context: &str,
    error: &anyhow::Error,
) -> PolicyDecision {
    counters.failures.fetch_add(1, Ordering::Relaxed);
    match failure_policy {
        FailurePolicy::FailOpen => {
            tracing::warn!(error = %error, context = %context, "persistence failed (fail-open)");
            PolicyDecision::Continue
        }
        FailurePolicy::FailClosed => {
            tracing::error!(error = %error, context = %context, "persistence failed (fail-closed) — terminating");
            PolicyDecision::Terminate(format!("persistence failed: {error}"))
        }
    }
}
