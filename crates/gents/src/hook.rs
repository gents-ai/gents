use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::llm::message::{Message, ToolResult};
use crate::llm::tool::ToolDyn;
use crate::llm::{HookAction, ToolCallHookAction};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::background_tools::LiveToolOutputRegistry;
use crate::meta_tools::selected_remote_identity;
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
    tools: HashMap<String, Arc<dyn ToolDyn>>,
    allowlist: Vec<String>,
    timeouts: crate::tool_surface::BackgroundTimeouts,
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
                registry_tools.insert(name, Arc::from(tool));
            }
        }
        let mut allowlist = allowed.into_iter().collect::<Vec<_>>();
        allowlist.sort();
        Self {
            inner: Arc::new(BackgroundToolRegistryInner {
                tools: registry_tools,
                allowlist,
                timeouts: Default::default(),
            }),
        }
    }

    /// Register the behavior's allowed background tools with its configured
    /// lifetimes and observation waits.
    pub(crate) fn from_config(
        tools: Vec<Box<dyn ToolDyn>>,
        config: &crate::tool_surface::BackgroundToolConfig,
    ) -> Self {
        let registry = Self::from_tools(tools, &config.allowlist);
        let inner = Arc::into_inner(registry.inner)
            .expect("a freshly built background registry has one owner");
        Self {
            inner: Arc::new(BackgroundToolRegistryInner {
                timeouts: config.timeouts.clone(),
                ..inner
            }),
        }
    }

    pub(crate) fn timeouts(&self) -> &crate::tool_surface::BackgroundTimeouts {
        &self.inner.timeouts
    }

    pub(crate) fn get(&self, tool_name: &str) -> Option<Arc<dyn ToolDyn>> {
        self.inner.tools.get(tool_name).cloned()
    }

    pub(crate) fn allowlist(&self) -> Vec<String> {
        self.inner.allowlist.clone()
    }
}

#[derive(Clone)]
struct BackgroundExecution {
    cancellation_token: CancellationToken,
    completion_tx: watch::Sender<bool>,
}

#[derive(Clone, Default)]
struct BackgroundLiveOutputState {
    registry: LiveToolOutputRegistry,
}

impl BackgroundLiveOutputState {
    async fn writer_for(
        &self,
        tool_call_id: impl Into<String>,
    ) -> crate::background_tools::LiveToolOutputWriter {
        self.registry.writer_for(tool_call_id).await
    }

    async fn canonical_writer_for(
        &self,
        binding: crate::tool_call_lifecycle::delivery::ToolOutputBinding,
    ) -> crate::background_tools::LiveToolOutputWriter {
        let tool_call_doc_id = binding.tool_call_doc_id.clone();
        self.registry
            .canonical_writer_for(tool_call_doc_id, Arc::new(binding))
            .await
    }

    async fn remove(&self, tool_call_id: &str) {
        self.registry.remove(tool_call_id).await;
    }
}

/// Process-wide volatile state for ordinary background tool calls.
///
/// Sharing this registry across request hooks keeps both cancellation tokens
/// and live output buffers reachable until each process becomes terminal.
#[derive(Clone, Default)]
pub struct BackgroundExecutionRegistry {
    inner: Arc<std::sync::Mutex<HashMap<String, BackgroundExecution>>>,
    live_outputs: BackgroundLiveOutputState,
    process_records: crate::managed_exec::ownership::ProcessRecordStore,
}

/// Bound on waiting for a signalled live worker to release its execution.
const LIVE_STOP_RELEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

impl BackgroundExecutionRegistry {
    /// Keep durable host records of spawned background processes in `dir`,
    /// which must belong to this runtime's exclusively locked store. Records
    /// are otherwise volatile: a restarted runtime cannot prove it owns a
    /// surviving process and settles its row as lost.
    pub fn with_process_records(mut self, dir: PathBuf) -> Self {
        self.process_records = crate::managed_exec::ownership::ProcessRecordStore::Durable(dir);
        self
    }

    /// Recorder installed around one background execution. Each spawned
    /// process replaces the execution's record.
    pub(crate) fn process_recorder(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> crate::managed_exec::ownership::ProcessRecorder {
        let store = self.process_records.clone();
        let tool_call_id = tool_call_id.to_owned();
        let tool_call_doc_id = tool_call_doc_id.to_owned();
        crate::managed_exec::ownership::ProcessRecorder::new(move |identity| {
            let record = crate::managed_exec::ownership::ProcessRecord {
                tool_call_id: tool_call_id.clone(),
                tool_call_doc_id: tool_call_doc_id.clone(),
                identity,
            };
            if let Err(error) = store.write(&record) {
                tracing::warn!(
                        tool_call_id = %record.tool_call_id,
                        %error,
                    "failed to record background process ownership; a runtime restart will settle it as lost"
                );
            }
        })
    }

    pub(crate) fn process_record(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> Option<crate::managed_exec::ownership::ProcessRecord> {
        self.process_records
            .read(tool_call_id)
            .filter(|record| record.tool_call_doc_id == tool_call_doc_id)
    }

    pub(crate) fn process_record_list(&self) -> Vec<crate::managed_exec::ownership::ProcessRecord> {
        self.process_records.list()
    }

    /// Forgets a finished execution's record once its leader is observed
    /// gone. A leader that outlives its execution keeps the record so a later
    /// sweep can stop it.
    pub(crate) async fn release_process_record(&self, tool_call_id: &str, tool_call_doc_id: &str) {
        if let Some(record) = self.process_record(tool_call_id, tool_call_doc_id) {
            let observed = record
                .identity
                .await_leader_exit(crate::managed_exec::ownership::KILL_GRACE)
                .await;
            if observed == crate::managed_exec::ownership::ProcessObservation::Running {
                tracing::warn!(
                    tool_call_id,
                    pid = record.identity.pid,
                    "background process outlived its execution; keeping its record"
                );
                return;
            }
        }
        self.process_records.remove(tool_call_id);
    }

    pub(crate) fn forget_process_record(&self, tool_call_id: &str) {
        self.process_records.remove(tool_call_id);
    }

    /// Stops one background execution through its owner and reports the
    /// verdict (Lean `ManagedExec.stopOutcome`). A live worker in this
    /// runtime is proven ownership: it is signalled and must release its
    /// execution, and any recorded group must then be observed empty. With
    /// no live worker, only a durable record whose leader matches proves
    /// ownership. The caller persists the row's terminal state first.
    pub(crate) async fn stop_execution(
        &self,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> crate::managed_exec::ProcessStopOutcome {
        use crate::managed_exec::ownership::ProcessObservation;
        use crate::managed_exec::ProcessStopOutcome;
        let record = self.process_record(tool_call_id, tool_call_doc_id);
        if self.cancel(tool_call_id).await {
            let released = tokio::time::timeout(
                LIVE_STOP_RELEASE_WAIT,
                self.wait_for_completion(tool_call_id),
            )
            .await
            .is_ok();
            let after = if !released {
                ProcessObservation::Running
            } else if let Some(record) = record {
                record
                    .identity
                    .await_signalled_group_exit(crate::managed_exec::ownership::KILL_GRACE)
                    .await
            } else {
                ProcessObservation::Exited
            };
            return ProcessStopOutcome::from_observations(ProcessObservation::Running, after);
        }
        let Some(record) = record else {
            return ProcessStopOutcome::NotOwned;
        };
        let (before, after) = record.identity.stop().await;
        ProcessStopOutcome::from_observations(before, after)
    }

    /// Authorized observation of the runtime-owned output buffer. Adapters
    /// use the same output and identity boundary as read_process, but read one
    /// complete retained snapshot without changing the model's page budget.
    pub async fn read_process_output_snapshot(
        &self,
        node: &EmbeddedNode,
        session_id: &str,
        agent_did: &str,
        requester_did: Option<&str>,
        tool_call_id: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let caller = crate::background_tools::ProcessControlScope {
            request_id: String::new(),
            session_id: session_id.to_owned(),
            agent_did: agent_did.to_owned(),
            requester_did: requester_did.map(str::to_owned),
        };
        let result = crate::background_tools::read_tool_output_slice(
            node,
            &caller,
            &self.live_outputs.registry,
            tool_call_id,
            0,
            usize::MAX,
        )
        .await?;
        Ok(match result {
            crate::background_tools::ReadToolOutputOutcome::Found(page) => {
                Some(serde_json::to_value(page)?)
            }
            _ => None,
        })
    }

    pub async fn cancel(&self, tool_call_id: &str) -> bool {
        let Some(execution) = self.lock_executions().get(tool_call_id).cloned() else {
            return false;
        };
        execution.cancellation_token.cancel();
        true
    }

    pub(crate) fn reserve(
        &self,
        tool_call_id: String,
        cancellation_token: CancellationToken,
    ) -> BackgroundExecutionReservation {
        let (completion_tx, _) = watch::channel(false);
        self.lock_executions().insert(
            tool_call_id.clone(),
            BackgroundExecution {
                cancellation_token,
                completion_tx,
            },
        );
        BackgroundExecutionReservation {
            registry: self.clone(),
            tool_call_id,
            armed: true,
        }
    }

    #[cfg(test)]
    pub(crate) async fn remove(&self, tool_call_id: &str) {
        self.remove_now(tool_call_id);
    }

    /// Returns whether this process still owns a live background execution.
    pub(crate) async fn contains(&self, tool_call_id: &str) -> bool {
        self.lock_executions().contains_key(tool_call_id)
    }

    /// Waits until this process releases ownership of a background execution.
    ///
    /// Normal worker completion releases ownership after durable terminal
    /// persistence, completion projection, and live-output cleanup. An
    /// unexpected task panic or abort drops the same ownership guard earlier,
    /// allowing recovery to observe the abandoned durable execution.
    ///
    /// Subscribing while holding the registry lock makes the boundary
    /// missed-event-safe: an execution removed before this call returns
    /// immediately, while an execution removed afterward signals this waiter.
    pub async fn wait_for_completion(&self, tool_call_id: &str) {
        let mut completion_rx = {
            let executions = self.lock_executions();
            let Some(execution) = executions.get(tool_call_id) else {
                return;
            };
            execution.completion_tx.subscribe()
        };
        while !*completion_rx.borrow_and_update() {
            if completion_rx.changed().await.is_err() {
                break;
            }
        }
    }

    fn remove_now(&self, tool_call_id: &str) {
        if let Some(execution) = self.lock_executions().remove(tool_call_id) {
            execution.completion_tx.send_replace(true);
        }
    }

    fn lock_executions(&self) -> std::sync::MutexGuard<'_, HashMap<String, BackgroundExecution>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Cancellation-safe ownership reservation for the durable-running handoff.
/// Dropping the caller future before `tokio::spawn` transfers ownership removes
/// the volatile entry synchronously, so periodic recovery can see the orphan.
pub(crate) struct BackgroundExecutionReservation {
    registry: BackgroundExecutionRegistry,
    tool_call_id: String,
    armed: bool,
}

impl BackgroundExecutionReservation {
    #[cfg(test)]
    pub(crate) fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for BackgroundExecutionReservation {
    fn drop(&mut self) {
        if self.armed {
            self.registry.remove_now(&self.tool_call_id);
        }
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
    current_request_doc_id: Option<String>,
    current_requester_did: Option<String>,
    request_deadline_at: Option<DateTime<Utc>>,
    sequence: u32,
    transcript_turn: TranscriptTurnState,
    persisted_tool_result_keys: HashSet<String>,
    persisted_tool_result_message_sequences: HashMap<String, u32>,
    tool_result_identities: HashMap<String, ToolResultIdentity>,
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
    accepted_tool_calls: Arc<Mutex<HashMap<String, crate::streaming::AcceptedToolCall>>>,
    background_tool_registry: BackgroundToolRegistry,
    background_executions: BackgroundExecutionRegistry,
    background_live_outputs: BackgroundLiveOutputState,
    operator_tool_root: Option<PathBuf>,
    remote_tools: Option<crate::document_config::RemoteTools>,
    goal_tools_enabled: bool,
    goal_creation_enabled: bool,
    output_obligation_gate: Option<crate::agent::output_obligation::OutputObligationGate>,
}

enum PolicyDecision {
    Continue,
    Terminate(String),
}

impl DefraSessionHook {
    /// Register provider-published physical calls before dispatch.  The next
    /// matching hook invocation adopts, rather than creates, that exact row.
    pub(crate) async fn adopt_accepted_tool_calls(
        &self,
        calls: Vec<(String, crate::streaming::AcceptedToolCall)>,
    ) -> anyhow::Result<()> {
        let mut pending = self.accepted_tool_calls.lock().await;
        for (internal_id, call) in calls {
            anyhow::ensure!(
                pending.insert(internal_id.clone(), call).is_none(),
                "duplicate accepted tool binding for internal call {internal_id}"
            );
        }
        Ok(())
    }

    /// Bind a dispatch hook invocation to the exact provider-published tool
    /// row. Rig's internal call key is deliberately only the map key: the
    /// provider-native identity registered by `StreamProcessor` must agree
    /// with the immutable accepted header before anything can run.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn adopt_accepted_tool_dispatch(
        &self,
        internal_call_id: &str,
        provider_call_id: Option<&str>,
        request_id: &str,
        session_id: &str,
        tool_name: &str,
        args: &str,
        deadline_at: DateTime<Utc>,
        await_mode: AwaitMode,
        cancel_policy: crate::tool_call_lifecycle::CancelPolicy,
    ) -> anyhow::Result<ToolCallLifecycle> {
        let registered = self
            .state
            .lock()
            .await
            .tool_result_identities
            .get(internal_call_id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "tool dispatch lacks StreamProcessor native identity for internal call {internal_call_id}"
                )
            })?;
        let native_id = registered.result_id.ok_or_else(|| {
            anyhow::anyhow!(
                "tool dispatch lacks registered provider tool id for internal call {internal_call_id}"
            )
        })?;
        if let Some(provider_call_id) = provider_call_id {
            anyhow::ensure!(
                Some(provider_call_id) == registered.call_id.as_deref(),
                "dispatch provider call id does not match StreamProcessor identity"
            );
        }

        let request_doc_id = self
            .active_request_doc_id_for(request_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("accepted dispatch requires a physical request binding")
            })?;
        let accepted = self
            .accepted_tool_calls
            .lock()
            .await
            .remove(internal_call_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "tool dispatch lacks its provider-published AcceptedToolCall binding"
                )
            })?;
        anyhow::ensure!(
            accepted.id == native_id
                && accepted.call_id == registered.call_id
                && accepted.request_doc_id == request_doc_id
                && accepted.session_id == session_id
                && accepted.tool_name == tool_name,
            "accepted tool binding does not match registered provider dispatch identity"
        );
        let selected = self
            .remote_tools
            .as_ref()
            .and_then(|remote| selected_remote_identity(tool_name, args, remote));
        Ok(ToolCallLifecycle::from_accepted(
            self.node.clone(),
            self.agent_did.clone(),
            self.active_requester_did().await,
            accepted,
            deadline_at,
            await_mode,
            cancel_policy,
        )?
        .with_selected_tool_identity(selected))
    }

    #[cfg(test)]
    pub fn with_identity(
        node: Arc<EmbeddedNode>,
        _agent_name: &str,
        agent_did: &str,
        failure_policy: FailurePolicy,
    ) -> Self {
        let background_executions = BackgroundExecutionRegistry::default();
        let background_live_outputs = background_executions.live_outputs.clone();
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
                session_id: Some(uuid::Uuid::new_v4().to_string()),
                current_request_id: None,
                current_request_doc_id: None,
                current_requester_did: None,
                request_deadline_at: None,
                sequence: 0,
                transcript_turn: TranscriptTurnState::Idle,
                persisted_tool_result_keys: HashSet::new(),
                persisted_tool_result_message_sequences: HashMap::new(),
                tool_result_identities: HashMap::new(),
            })),
            in_flight_lifecycles: Arc::new(Mutex::new(HashMap::new())),
            accepted_tool_calls: Arc::new(Mutex::new(HashMap::new())),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_executions,
            background_live_outputs,
            operator_tool_root: None,
            remote_tools: None,
            goal_tools_enabled: false,
            goal_creation_enabled: false,
            output_obligation_gate: None,
        }
    }

    pub async fn resume_with_identity_policy(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        _agent_name: &str,
        agent_did: &str,
        requester_did: Option<&str>,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        session::require_session(&node, agent_did, session_id, requester_did).await?;
        let max_seq = session::max_sequence(&node, session_id, agent_did, requester_did).await?;
        let background_executions = BackgroundExecutionRegistry::default();
        let background_live_outputs = background_executions.live_outputs.clone();

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
                current_request_doc_id: None,
                current_requester_did: None,
                request_deadline_at: None,
                sequence: max_seq,
                transcript_turn: TranscriptTurnState::Idle,
                persisted_tool_result_keys: HashSet::new(),
                persisted_tool_result_message_sequences: HashMap::new(),
                tool_result_identities: HashMap::new(),
            })),
            in_flight_lifecycles: Arc::new(Mutex::new(HashMap::new())),
            accepted_tool_calls: Arc::new(Mutex::new(HashMap::new())),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_executions,
            background_live_outputs,
            operator_tool_root: None,
            remote_tools: None,
            goal_tools_enabled: false,
            goal_creation_enabled: false,
            output_obligation_gate: None,
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
        self.background_live_outputs = registry.live_outputs.clone();
        self.background_executions = registry;
        self
    }

    pub fn with_operator_tool_root(mut self, root: Option<PathBuf>) -> Self {
        self.operator_tool_root = root;
        self
    }

    /// Install the already-intersected goal authority for this request hook.
    /// Defaults are deny-all so a forgotten wiring site cannot turn the
    /// pre-dispatch persistence hook into an authority bypass.
    pub fn with_goal_tool_authority(
        mut self,
        goal_tools_enabled: bool,
        goal_creation_enabled: bool,
    ) -> Self {
        self.goal_tools_enabled = goal_tools_enabled;
        self.goal_creation_enabled = goal_tools_enabled && goal_creation_enabled;
        self
    }

    /// Share this request's configured output gate with the owned completion loop.
    pub(crate) fn with_output_obligation_gate(
        mut self,
        gate: Option<crate::agent::output_obligation::OutputObligationGate>,
    ) -> Self {
        self.output_obligation_gate = gate;
        self
    }

    pub fn set_operator_tool_root(&mut self, root: Option<PathBuf>) {
        self.operator_tool_root = root;
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

    /// Call-hook failure never authorizes ordinary dispatch: the election may
    /// have committed without a receipt, or a control tool may already have
    /// performed its effect. Result-write policy cannot grant that authority.
    fn on_tool_persistence_error(
        &self,
        context: &str,
        error: &anyhow::Error,
    ) -> ToolCallHookAction {
        self.counters.failures.fetch_add(1, Ordering::Relaxed);
        tracing::error!(error = %error, context = %context, "tool call persistence failed; dispatch denied");
        ToolCallHookAction::Terminate {
            reason: format!("tool call persistence failed: {error}"),
        }
    }

    pub async fn session_id(&self) -> Option<String> {
        self.state.lock().await.session_id.clone()
    }

    pub async fn set_active_request_lineage(
        &self,
        request_id: Option<String>,
        requester_did: Option<String>,
    ) -> anyhow::Result<()> {
        let request_doc_id = match request_id.as_deref() {
            Some(request_id) => Some(
                self.request_doc_id_for_request(request_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("active AgentRequest {request_id} not found"))?,
            ),
            None => None,
        };
        self.set_active_request_binding(request_id, request_doc_id, requester_did)
            .await;
        Ok(())
    }

    pub async fn set_active_request_binding(
        &self,
        request_id: Option<String>,
        request_doc_id: Option<String>,
        requester_did: Option<String>,
    ) {
        if request_id.is_some() != request_doc_id.is_some() {
            tracing::warn!(
                has_request_id = request_id.is_some(),
                has_request_doc_id = request_doc_id.is_some(),
                "rejected half-bound active request lineage",
            );
            let mut state = self.state.lock().await;
            state.current_request_id = None;
            state.current_request_doc_id = None;
            state.current_requester_did = None;
            return;
        }
        let mut state = self.state.lock().await;
        state.current_request_id = request_id;
        state.current_request_doc_id = request_doc_id;
        state.current_requester_did = requester_did;
    }

    async fn active_requester_did(&self) -> Option<String> {
        self.state.lock().await.current_requester_did.clone()
    }

    async fn active_request_doc_id(&self) -> Option<String> {
        self.state.lock().await.current_request_doc_id.clone()
    }

    async fn active_request_doc_id_for(&self, request_id: &str) -> anyhow::Result<Option<String>> {
        // Hooks may also be used for session-only transcript capture, where
        // there is deliberately no AgentRequest to bind. Keep that state
        // wholly unbound instead of trying to resolve an empty logical id.
        if request_id.trim().is_empty() {
            return Ok(None);
        }
        let state = self.state.lock().await;
        if state.current_request_id.as_deref() != Some(request_id) {
            anyhow::bail!("active request changed while resolving document provenance");
        }
        let doc_id = state
            .current_request_doc_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("active request is missing its physical document id"))?;
        Ok(Some(doc_id))
    }

    async fn request_doc_id_for_request(&self, request_id: &str) -> anyhow::Result<Option<String>> {
        crate::request_binding::resolve_request_doc_id(self.node.as_ref(), request_id).await
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

    pub(crate) fn with_remote_tools(
        mut self,
        tools: Option<crate::document_config::RemoteTools>,
    ) -> Self {
        self.remote_tools = tools;
        self
    }

    pub(crate) async fn foreground_live_output_writer(
        &self,
        internal_call_id: &str,
    ) -> crate::background_tools::LiveToolOutputWriter {
        let binding = self
            .in_flight_lifecycles
            .lock()
            .await
            .get(internal_call_id)
            .and_then(|lifecycle| lifecycle.tool_output_binding().ok());
        if let Some(binding) = binding {
            return self
                .background_live_outputs
                .canonical_writer_for(binding)
                .await;
        }
        // The caller will fail terminalization without a physical accepted
        // binding; this registration has no payload fallback.
        self.background_live_outputs
            .writer_for(internal_call_id)
            .await
    }

    pub(crate) async fn release_live_output(&self, tool_call_id: &str) {
        self.background_live_outputs.remove(tool_call_id).await;
    }

    pub(crate) async fn timeout_expired_tool_calls(&self) -> anyhow::Result<usize> {
        let lifecycles = {
            let now = Utc::now();
            let mut map = self.in_flight_lifecycles.lock().await;
            let expired_ids = map
                .iter()
                .filter_map(|(id, lifecycle)| {
                    lifecycle.is_deadline_expired(now).then(|| id.clone())
                })
                .collect::<Vec<_>>();

            expired_ids
                .into_iter()
                .filter_map(|id| map.remove(&id))
                .collect::<Vec<_>>()
        };

        let count = lifecycles.len();
        for mut lifecycle in lifecycles {
            if lifecycle.is_subagent_bridge() && lifecycle.await_mode() != AwaitMode::Foreground {
                tracing::debug!(
                    "leaving background subagent bridge running after parent deadline sweep"
                );
            } else {
                // Foreground subagent bridges take the same deadline
                // transition as native tools: `timedOut`, never a fabricated
                // `ChildTerminal::Dead` — the child may still be live, and its
                // terminalization belongs to the subagent-liveness sweep
                // (#1002; Lean `ToolExecution.Transition.timeout`).
                let _ = lifecycle.timeout().await?;
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

        let mut count = lifecycles.len();
        for mut lifecycle in lifecycles {
            let dispatch = lifecycle
                .cancel_during_run_with_cascade_dispatch(CancelCause::Interrupted, &self.agent_did)
                .await?;
            if lifecycle.is_cancelled() {
                if let Some(dispatch) = dispatch {
                    if let CascadeDispatch::Local { intent, child } = dispatch {
                        if let Err(error) = crate::interrupt::interrupt_request_by_doc_id(
                            &self.node,
                            child
                                .doc_id
                                .as_deref()
                                .expect("verified physical cascade child"),
                            child
                                .agent_did
                                .as_deref()
                                .expect("verified local child principal"),
                            child.requester_did.as_deref(),
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
        // spawn_process lifecycles live in the background registry, not the
        // foreground map. Explicit interruption cascades to this exact physical
        // parent's native background work; ordinary completion still detaches
        // execution from the parent's deadline as before.
        if let Some(parent_doc) = self.active_request_doc_id().await {
            let query = format!(
                r#"{{ AgentToolCall(filter: {{ agent_did: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "running" }}, await_mode: {{ _eq: "background" }}, cancel_policy: {{ _eq: "cascade" }}, child_request_id: {{ _eq: null }} }}) {{ session_id tool_call_id }} }}"#,
                crate::graphql::escape_graphql_string(&self.agent_did),
                crate::graphql::escape_graphql_string(&parent_doc),
            );
            let response = self.node.execute(&query).await;
            anyhow::ensure!(
                !response.has_errors(),
                "load interrupted background tools: {:?}",
                response.errors
            );
            for row in response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentToolCall"))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                let session_id = row["session_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("background tool omitted session"))?;
                let tool_call_id = row["tool_call_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("background tool omitted identity"))?;
                if matches!(
                    crate::tool_control::cancel_background_tool_call_with_cause(
                        self.node.clone(),
                        &self.background_executions,
                        &self.agent_did,
                        session_id,
                        tool_call_id,
                        CancelCause::Interrupted,
                    )
                    .await?,
                    crate::tool_control::CancelBackgroundToolCallOutcome::Cancelled { .. }
                ) {
                    count += 1;
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

    pub async fn close(&self) -> anyhow::Result<()> {
        let session_id = self.state.lock().await.session_id.clone();
        if let Some(id) = session_id {
            session::close_session(
                &self.node,
                &self.agent_did,
                &id,
                self.active_requester_did().await.as_deref(),
            )
            .await?;
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

#[async_trait::async_trait]
impl
    gents_loop::session_hook::CanonicalSessionHook<
        crate::streaming::AcceptedToolCall,
        crate::streaming::SpawnAdmissionPlan,
    > for DefraSessionHook
{
    async fn preplan_spawn_admissions(
        &self,
        message: &Message,
        internal_call_ids: &[String],
    ) -> anyhow::Result<Vec<crate::streaming::SpawnAdmissionPlan>> {
        DefraSessionHook::preplan_spawn_admissions(self, message, internal_call_ids).await
    }

    async fn adopt_accepted_tool_calls(
        &self,
        calls: Vec<(String, crate::streaming::AcceptedToolCall)>,
    ) -> anyhow::Result<()> {
        DefraSessionHook::adopt_accepted_tool_calls(self, calls).await
    }
}

#[async_trait::async_trait]
impl gents_loop::session_hook::SessionHook for DefraSessionHook {
    async fn on_completion_call_with_context(
        &self,
        prompt: &Message,
        history: &[Message],
        context: Option<&Message>,
    ) -> HookAction {
        DefraSessionHook::on_completion_call_with_context(self, prompt, history, context).await
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        DefraSessionHook::on_tool_call(self, tool_name, tool_call_id, internal_call_id, args).await
    }

    async fn on_tool_admission_rejected(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &gents_loop::tool_call_lifecycle::ToolOutcome,
    ) -> HookAction {
        DefraSessionHook::on_tool_admission_rejected(
            self,
            tool_name,
            tool_call_id,
            internal_call_id,
            args,
            outcome,
        )
        .await
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        outcome: &gents_loop::tool_call_lifecycle::ToolOutcome,
    ) -> HookAction {
        DefraSessionHook::on_tool_result(
            self,
            tool_name,
            tool_call_id,
            internal_call_id,
            args,
            outcome,
        )
        .await
    }

    async fn foreground_live_output_writer(
        &self,
        internal_call_id: &str,
    ) -> gents_loop::live_output::LiveToolOutputWriter {
        DefraSessionHook::foreground_live_output_writer(self, internal_call_id).await
    }

    async fn session_id(&self) -> Option<String> {
        DefraSessionHook::session_id(self).await
    }

    async fn register_stream_tool_call_identity(
        &self,
        internal_call_id: &str,
        result_id: &str,
        call_id: Option<&str>,
    ) {
        DefraSessionHook::register_stream_tool_call_identity(
            self,
            internal_call_id,
            result_id,
            call_id,
        )
        .await
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
