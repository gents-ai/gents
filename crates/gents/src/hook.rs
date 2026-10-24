use gents_protocol::error_chain::ErrorChain;
use rapidhash::{HashMapExt, RapidHashMap, RapidHashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::llm::message::Message;
use crate::llm::tool::ToolDyn;
use crate::llm::{HookAction, ToolCallHookAction};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::background_tools::LiveToolOutputRegistry;
use crate::meta_tools::selected_remote_identity;
use crate::session;
use crate::tool_call_lifecycle::{AwaitMode, CancelCause, InterruptDisposition, ToolCallLifecycle};
use crate::truncation::TruncationLimits;

pub(crate) mod persistence;
#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error)]
pub(crate) enum HookError {
    #[error("duplicate accepted tool binding for internal call {internal_call_id}")]
    DuplicateAcceptedToolBinding { internal_call_id: String },
    #[error("active request changed while resolving document provenance")]
    ActiveRequestChanged,
    #[error("active request is missing its physical document id")]
    ActiveRequestMissingDocId,
    #[error("decode background row count")]
    DecodeBackgroundRowCount(#[source] serde_json::Error),
    #[error("session hook missing session id")]
    MissingSessionId,
    #[error("tool call is missing its active request id")]
    MissingActiveRequestId,
    #[error("tool call is missing its active request document id")]
    MissingActiveRequestDocId,
    #[error("tool call is missing its request deadline")]
    MissingRequestDeadline,
    #[error("plugin effect dispatch does not match its admitted input")]
    PluginEffectDispatchMismatch,
    #[error(
        "tool dispatch lacks StreamProcessor native identity for internal call {internal_call_id}"
    )]
    DispatchMissingStreamIdentity { internal_call_id: String },
    #[error(
        "tool dispatch lacks registered provider tool id for internal call {internal_call_id}"
    )]
    DispatchMissingProviderToolId { internal_call_id: String },
    #[error("dispatch provider call id does not match StreamProcessor identity")]
    DispatchProviderCallIdMismatch,
    #[error("accepted dispatch requires a physical request binding")]
    DispatchMissingRequestBinding,
    #[error("tool dispatch lacks its provider-published AcceptedToolCall binding")]
    DispatchMissingAcceptedBinding,
    #[error("accepted tool binding does not match registered provider dispatch identity")]
    AcceptedBindingMismatch,
    #[error(transparent)]
    ToolCallLifecycle(#[from] crate::tool_call_lifecycle::ToolCallLifecycleError),
}

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
    tools: RapidHashMap<String, Arc<dyn ToolDyn>>,
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
            .collect::<RapidHashSet<_>>();
        let mut registry_tools = RapidHashMap::new();
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
    tool_call_doc_id: String,
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

#[derive(Clone, Default)]
pub struct BackgroundExecutionRegistry {
    inner: Arc<std::sync::Mutex<RapidHashMap<String, BackgroundExecution>>>,
    live_outputs: BackgroundLiveOutputState,
    process_records: crate::managed_exec::ownership::ProcessRecordStore,
    task_hook_records: crate::task_hooks::TaskHookRecordStore,
}

const LIVE_STOP_RELEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

impl BackgroundExecutionRegistry {
    /// Keep durable host records of spawned background processes in `dir`,
    /// which must belong to this runtime's exclusively locked store; a
    /// restarted runtime otherwise cannot prove it owns a surviving process.
    pub fn with_process_records(mut self, dir: PathBuf) -> Self {
        self.process_records = crate::managed_exec::ownership::ProcessRecordStore::Durable(dir);
        self
    }

    pub fn with_task_hook_records(mut self, dir: PathBuf) -> Self {
        self.task_hook_records = crate::task_hooks::TaskHookRecordStore::durable(dir);
        self
    }

    pub(crate) fn task_hook_records(&self) -> &crate::task_hooks::TaskHookRecordStore {
        &self.task_hook_records
    }

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

    pub(crate) fn try_process_record_list(
        &self,
    ) -> Result<
        Vec<crate::managed_exec::ownership::ProcessRecord>,
        crate::managed_exec::ownership::ManagedExecError,
    > {
        self.process_records.try_list()
    }

    pub(crate) async fn release_process_record(&self, tool_call_id: &str, tool_call_doc_id: &str) {
        if let Some(record) = self.process_record(tool_call_id, tool_call_doc_id) {
            let observed = record
                .identity
                .await_leader_exit(crate::managed_exec::ownership::KILL_GRACE)
                .await;
            if observed.retain_process_record() {
                tracing::warn!(
                    tool_call_id,
                    pid = record.identity.pid,
                    observation = ?observed,
                    "background process group is not proven exited; keeping its record"
                );
                return;
            }
            self.process_records.remove(tool_call_id);
        }
    }

    pub(crate) fn forget_process_record(&self, tool_call_id: &str) {
        if let Some(record) = self.process_records.read(tool_call_id) {
            let observed = record.identity.observe();
            if observed.retain_process_record() {
                tracing::warn!(
                    tool_call_id,
                    pid = record.identity.pid,
                    observation = ?observed,
                    "background process group is not proven exited; keeping its record"
                );
                return;
            }
            self.process_records.remove(tool_call_id);
        }
    }

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

    pub async fn read_process_output_snapshot(
        &self,
        node: &EmbeddedNode,
        session_id: &str,
        node_did: &str,
        requester_did: Option<&str>,
        tool_call_id: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let caller = crate::background_tools::ProcessControlScope {
            request_id: String::new(),
            session_id: session_id.to_owned(),
            node_did: node_did.to_owned(),
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
        tool_call_doc_id: String,
        cancellation_token: CancellationToken,
    ) -> BackgroundExecutionReservation {
        let (completion_tx, _) = watch::channel(false);
        self.lock_executions().insert(
            tool_call_id.clone(),
            BackgroundExecution {
                cancellation_token,
                completion_tx,
                tool_call_doc_id,
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

    pub(crate) async fn contains(&self, tool_call_id: &str) -> bool {
        self.lock_executions().contains_key(tool_call_id)
    }

    pub(crate) fn active_tool_call_doc_ids(&self) -> Vec<String> {
        self.lock_executions()
            .values()
            .map(|execution| execution.tool_call_doc_id.clone())
            .collect()
    }

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

    fn lock_executions(
        &self,
    ) -> std::sync::MutexGuard<'_, RapidHashMap<String, BackgroundExecution>> {
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
    tool_result_identities: RapidHashMap<String, ToolResultIdentity>,
}

impl SessionState {
    // Tool-result messages stay in the assistant turn so parallel calls share
    // its sequence (Lean: `Transcript.parallel_results_complete_independently`).
    fn reset_after_user_message(&mut self) {
        self.transcript_turn = TranscriptTurnState::Idle;
    }

    fn begin_or_continue_assistant_turn(&mut self) -> u32 {
        match self.transcript_turn {
            TranscriptTurnState::AssistantBuilding { sequence } => sequence,
            TranscriptTurnState::Idle => {
                self.sequence += 1;
                let sequence = self.sequence;
                self.transcript_turn = TranscriptTurnState::AssistantBuilding { sequence };
                sequence
            }
        }
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
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| (!value.is_empty()).then_some(value))
}

#[derive(Clone)]
pub struct DefraSessionHook {
    node: Arc<EmbeddedNode>,
    node_did: String,
    truncation_limits: TruncationLimits,
    failure_policy: FailurePolicy,
    counters: Arc<HookCounters>,
    state: Arc<Mutex<SessionState>>,
    in_flight_lifecycles: Arc<Mutex<RapidHashMap<String, ToolCallLifecycle>>>,
    accepted_tool_calls: Arc<Mutex<RapidHashMap<String, crate::streaming::AcceptedToolCall>>>,
    effect_tool_calls: Arc<Mutex<RapidHashMap<String, (ToolCallLifecycle, String)>>>,
    effect_lifecycles: Arc<Mutex<RapidHashMap<String, ToolCallLifecycle>>>,
    background_tool_registry: BackgroundToolRegistry,
    background_executions: BackgroundExecutionRegistry,
    background_live_outputs: BackgroundLiveOutputState,
    operator_tool_root: Option<PathBuf>,
    remote_tools: Option<crate::document_config::RemoteTools>,
    goal_tools_enabled: bool,
    goal_creation_enabled: bool,
    job_tools: RapidHashMap<String, crate::plugin_job::JobBinding>,
    /// The runtime's job driver, whose handles step 0 of a job tool runs on.
    pub(crate) job_driver: Option<Arc<crate::plugin_job::driver::JobDriver>>,
    output_obligation_gate: Option<crate::agent::output_obligation::OutputObligationGate>,
    steering_input_source: Option<Arc<SteeringInputSource>>,
}

struct SteeringInputSource {
    request: crate::watcher::AgentRequest,
    verifier: crate::request_admission::AgentRequestAdmissionVerifier,
    natural_finish_allowed: bool,
}

enum PolicyDecision {
    Continue,
    Terminate(String),
}

impl DefraSessionHook {
    pub(crate) fn with_steering_inputs(
        mut self,
        request: crate::watcher::AgentRequest,
        verifier: crate::request_admission::AgentRequestAdmissionVerifier,
        natural_finish_allowed: bool,
    ) -> Self {
        self.steering_input_source = Some(Arc::new(SteeringInputSource {
            request,
            verifier,
            natural_finish_allowed,
        }));
        self
    }
    pub(crate) async fn adopt_accepted_tool_calls(
        &self,
        calls: Vec<(String, crate::streaming::AcceptedToolCall)>,
    ) -> Result<(), HookError> {
        let mut pending = self.accepted_tool_calls.lock().await;
        for (internal_id, call) in calls {
            if pending.insert(internal_id.clone(), call).is_some() {
                return Err(HookError::DuplicateAcceptedToolBinding {
                    internal_call_id: internal_id,
                });
            }
        }
        Ok(())
    }

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
    ) -> Result<ToolCallLifecycle, HookError> {
        if let Some((lifecycle, recorded_arguments)) =
            self.effect_tool_calls.lock().await.remove(internal_call_id)
        {
            if !(provider_call_id.is_none()
                && lifecycle.tool_name() == tool_name
                && lifecycle.request_id() == request_id
                && lifecycle.session_id() == session_id
                && recorded_arguments == args)
            {
                return Err(HookError::PluginEffectDispatchMismatch);
            }
            return Ok(lifecycle);
        }
        let registered = self
            .state
            .lock()
            .await
            .tool_result_identities
            .get(internal_call_id)
            .cloned()
            .ok_or_else(|| HookError::DispatchMissingStreamIdentity {
                internal_call_id: internal_call_id.to_owned(),
            })?;
        let native_id =
            registered
                .result_id
                .ok_or_else(|| HookError::DispatchMissingProviderToolId {
                    internal_call_id: internal_call_id.to_owned(),
                })?;
        if let Some(provider_call_id) = provider_call_id {
            if Some(provider_call_id) != registered.call_id.as_deref() {
                return Err(HookError::DispatchProviderCallIdMismatch);
            }
        }

        let request_doc_id = self
            .active_request_doc_id_for(request_id)
            .await?
            .ok_or(HookError::DispatchMissingRequestBinding)?;
        let accepted = self
            .accepted_tool_calls
            .lock()
            .await
            .remove(internal_call_id)
            .ok_or(HookError::DispatchMissingAcceptedBinding)?;
        if !(accepted.id == native_id
            && accepted.call_id == registered.call_id
            && accepted.request_doc_id == request_doc_id
            && accepted.session_id == session_id
            && accepted.tool_name == tool_name)
        {
            return Err(HookError::AcceptedBindingMismatch);
        }
        let selected = self
            .remote_tools
            .as_ref()
            .and_then(|remote| selected_remote_identity(tool_name, args, remote));
        Ok(ToolCallLifecycle::from_accepted(
            self.node.clone(),
            self.node_did.clone(),
            self.active_requester_did().await,
            accepted,
            deadline_at,
            await_mode,
        )?
        .with_selected_tool_identity(selected))
    }

    #[cfg(test)]
    pub fn with_identity(
        node: Arc<EmbeddedNode>,
        node_did: &str,
        failure_policy: FailurePolicy,
    ) -> Self {
        let background_executions = BackgroundExecutionRegistry::default();
        let background_live_outputs = background_executions.live_outputs.clone();
        Self {
            node,
            node_did: node_did.to_string(),
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
                tool_result_identities: RapidHashMap::new(),
            })),
            in_flight_lifecycles: Arc::new(Mutex::new(RapidHashMap::new())),
            accepted_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_lifecycles: Arc::new(Mutex::new(RapidHashMap::new())),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_executions,
            background_live_outputs,
            operator_tool_root: None,
            remote_tools: None,
            goal_tools_enabled: false,
            goal_creation_enabled: false,
            job_tools: RapidHashMap::new(),
            job_driver: None,
            output_obligation_gate: None,
            steering_input_source: None,
        }
    }

    pub async fn resume_with_identity_policy(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        node_did: &str,
        requester_did: Option<&str>,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        session::require_session(&node, node_did, session_id, requester_did).await?;
        let max_seq = session::max_sequence(&node, session_id, node_did, requester_did).await?;
        let background_executions = BackgroundExecutionRegistry::default();
        let background_live_outputs = background_executions.live_outputs.clone();

        Ok(Self {
            node,
            node_did: node_did.to_string(),
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
                tool_result_identities: RapidHashMap::new(),
            })),
            in_flight_lifecycles: Arc::new(Mutex::new(RapidHashMap::new())),
            accepted_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_lifecycles: Arc::new(Mutex::new(RapidHashMap::new())),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_executions,
            background_live_outputs,
            operator_tool_root: None,
            remote_tools: None,
            goal_tools_enabled: false,
            goal_creation_enabled: false,
            job_tools: RapidHashMap::new(),
            job_driver: None,
            output_obligation_gate: None,
            steering_input_source: None,
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

    pub fn with_goal_tool_authority(
        mut self,
        goal_tools_enabled: bool,
        goal_creation_enabled: bool,
    ) -> Self {
        self.goal_tools_enabled = goal_tools_enabled;
        self.goal_creation_enabled = goal_tools_enabled && goal_creation_enabled;
        self
    }

    /// Bind the Agent's job tools; a plugin child call naming one is refused.
    pub(crate) fn with_job_tools(
        mut self,
        job_tools: RapidHashMap<String, crate::plugin_job::JobBinding>,
    ) -> Self {
        self.job_tools = job_tools;
        self
    }

    /// Bind the runtime's job driver; a job tool is refused without it.
    pub(crate) fn with_job_driver(
        mut self,
        driver: Arc<crate::plugin_job::driver::JobDriver>,
    ) -> Self {
        self.job_driver = Some(driver);
        self
    }

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

    #[cfg(test)]
    async fn active_request_doc_id(&self) -> Option<String> {
        self.state.lock().await.current_request_doc_id.clone()
    }

    async fn active_request_doc_id_for(
        &self,
        request_id: &str,
    ) -> Result<Option<String>, HookError> {
        if request_id.trim().is_empty() {
            return Ok(None);
        }
        let state = self.state.lock().await;
        if state.current_request_id.as_deref() != Some(request_id) {
            return Err(HookError::ActiveRequestChanged);
        }
        let doc_id = state
            .current_request_doc_id
            .clone()
            .ok_or(HookError::ActiveRequestMissingDocId)?;
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
                .filter(|&(_, lifecycle)| lifecycle.is_deadline_expired(now))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();

            expired_ids
                .into_iter()
                .filter_map(|id| map.remove(&id))
                .collect::<Vec<_>>()
        };

        let count = lifecycles.len();
        for mut lifecycle in lifecycles {
            let _ = lifecycle.timeout().await?;
        }
        let effects = self
            .effect_lifecycles
            .lock()
            .await
            .iter()
            .filter(|(_, child)| child.is_deadline_expired(Utc::now()))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in effects {
            <Self as gents_loop::session_hook::SessionHook>::finish_tool_effect(
                self,
                &id,
                Some(&gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { deadline_at: None }),
            )
            .await
            .map_err(anyhow::Error::msg)?;
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

        let mut cancelled = 0;
        let mut first_error = None;
        for mut lifecycle in lifecycles {
            let applied = match lifecycle.interrupt_disposition() {
                InterruptDisposition::Cancel if lifecycle.is_running() => lifecycle
                    .cancel_during_run(CancelCause::Interrupted)
                    .await
                    .map(|won| cancelled += usize::from(won)),
                InterruptDisposition::Cancel | InterruptDisposition::Retain => Ok(()),
            };
            if let Err(error) = applied {
                first_error.get_or_insert(error);
            }
        }
        let effects = self
            .effect_lifecycles
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in effects {
            if let Err(error) = <Self as gents_loop::session_hook::SessionHook>::finish_tool_effect(
                self,
                &id,
                Some(&gents_loop::tool_call_lifecycle::ToolOutcome::Cancelled),
            )
            .await
            {
                first_error.get_or_insert(anyhow::Error::msg(error));
            }
        }
        first_error.map_or(Ok(cancelled), Err)
    }

    #[cfg(test)]
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
            lifecycle.fail(result, failure_class).await?;
        }
        Ok(count)
    }

    pub async fn close(&self) -> anyhow::Result<()> {
        let session_id = self.state.lock().await.session_id.clone();
        if let Some(id) = session_id {
            session::close_session(
                &self.node,
                &self.node_did,
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
impl gents_loop::session_hook::CanonicalSessionHook<crate::streaming::AcceptedToolCall>
    for DefraSessionHook
{
    async fn adopt_accepted_tool_calls(
        &self,
        calls: Vec<(String, crate::streaming::AcceptedToolCall)>,
    ) -> anyhow::Result<()> {
        Ok(DefraSessionHook::adopt_accepted_tool_calls(self, calls).await?)
    }
}

#[async_trait::async_trait]
impl gents_loop::session_hook::SessionHook for DefraSessionHook {
    type Error = anyhow::Error;

    async fn run_foreground_tool(
        &self,
        name: &str,
        internal_id: &str,
        call: gents_loop::session_hook::ForegroundCall,
    ) -> gents_loop::session_hook::ForegroundResult {
        self.run_backgroundable_foreground(name, internal_id, call)
            .await
    }

    async fn admit_tool_effect(
        &self,
        parent_internal_id: &str,
        ordinal: u32,
        tool_name: &str,
        arguments: &str,
    ) -> Result<String, String> {
        let parents = self.in_flight_lifecycles.lock().await;
        let parent = parents
            .get(parent_internal_id)
            .cloned()
            .ok_or_else(|| "plugin effect has no running parent".to_owned())?;
        drop(parents);
        let mut child = parent
            .admit_plugin_effect(ordinal, tool_name, arguments)
            .await
            .map_err(|error| {
                format!("cannot admit plugin effect: {}", ErrorChain(error.as_ref()))
            })?;
        let internal_id = child.plugin_effect_internal_id().to_owned();
        if !crate::plugin::tool_calls::supported(
            tool_name,
            &self
                .job_tools
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<String>>(),
        ) {
            let reason = format!(
                "{tool_name} controls request, session or process lifecycle; call it directly outside the plugin"
            );
            child
                .spawn_failed(
                    crate::tool_call_lifecycle::FailureClass::PolicyDenied,
                    &reason,
                )
                .await
                .map_err(|error| {
                    format!(
                        "cannot persist plugin effect refusal: {}",
                        ErrorChain(error.as_ref())
                    )
                })?;
            return Err(reason);
        }
        let mut pending = self.effect_tool_calls.lock().await;
        if pending.contains_key(&internal_id) {
            return Err("plugin effect is already awaiting dispatch".into());
        }
        self.effect_lifecycles
            .lock()
            .await
            .insert(internal_id.clone(), child.clone());
        pending.insert(internal_id.clone(), (child, arguments.to_owned()));
        Ok(internal_id)
    }

    async fn finish_tool_effect(
        &self,
        internal_id: &str,
        interruption: Option<&gents_loop::tool_call_lifecycle::ToolOutcome>,
    ) -> Result<(), String> {
        let Some(binding) = self
            .effect_lifecycles
            .lock()
            .await
            .get(internal_id)
            .cloned()
        else {
            return Ok(());
        };
        self.effect_tool_calls.lock().await.remove(internal_id);
        let result: anyhow::Result<()> = async {
            let mut current = ToolCallLifecycle::load_by_doc_id(
                self.node.clone(),
                binding
                    .doc_id()
                    .ok_or_else(|| anyhow::anyhow!("plugin child has no physical identity"))?,
                binding.node_did(),
                binding.session_id(),
                binding.requester_did(),
            )
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("plugin child disappeared before interruption settlement")
            })?;
            let Some(outcome) = interruption else {
                anyhow::ensure!(
                    current.is_terminal(),
                    "plugin child did not durably terminalize"
                );
                crate::tool_call_lifecycle::query::load_tool_call_result(
                    &crate::config_client::ConfigAccess::local(self.node.clone()),
                    current.doc_id().expect("loaded physical child"),
                    current.node_did(),
                    current.session_id(),
                    current.requester_did(),
                )
                .await?;
                self.in_flight_lifecycles.lock().await.remove(internal_id);
                return Ok(());
            };
            let cause = match outcome {
                gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { .. } => {
                    crate::tool_call_lifecycle::CancelCause::Deadline
                }
                _ => crate::tool_call_lifecycle::CancelCause::Interrupted,
            };
            match current.state() {
                crate::tool_call_lifecycle::ToolCallState::Pending => {
                    current.cancel_before_dispatch(cause).await?
                }
                crate::tool_call_lifecycle::ToolCallState::Running => {
                    if cause == crate::tool_call_lifecycle::CancelCause::Deadline {
                        current.timeout().await?;
                    } else {
                        current.cancel_during_run(cause).await?;
                    }
                }
                _ => {}
            }
            self.in_flight_lifecycles.lock().await.remove(internal_id);
            Ok(())
        }
        .await;
        if result.is_ok() {
            self.effect_lifecycles.lock().await.remove(internal_id);
        }
        result.map_err(|error| {
            format!(
                "cannot settle plugin effect: {}",
                ErrorChain(error.as_ref())
            )
        })
    }

    async fn pending_steering_inputs(
        &self,
    ) -> anyhow::Result<Vec<gents_loop::loop_stream::FoldedPrompt>> {
        let Some(source) = self.steering_input_source.as_ref() else {
            return Ok(Vec::new());
        };
        anyhow::ensure!(
            crate::lifecycle::RequestLifecycle::input_publication_before_deadline(
                Utc::now(),
                self.state.lock().await.request_deadline_at,
            ),
            "request deadline expired before steering intake"
        );
        crate::lifecycle::queue::steering_inputs(&self.node, &source.request, &source.verifier)
            .await
    }

    async fn finish_steering_snapshot(
        &self,
    ) -> anyhow::Result<Option<gents_loop::loop_stream::SteeringSnapshot>> {
        let Some(source) = self
            .steering_input_source
            .as_ref()
            .filter(|source| source.natural_finish_allowed)
        else {
            return Ok(None);
        };
        anyhow::ensure!(
            crate::lifecycle::RequestLifecycle::input_publication_before_deadline(
                Utc::now(),
                self.state.lock().await.request_deadline_at,
            ),
            "request deadline expired before steering intake"
        );
        Ok(Some(
            crate::lifecycle::queue::steering_snapshot(
                &self.node,
                &source.request,
                &source.verifier,
            )
            .await?,
        ))
    }

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
