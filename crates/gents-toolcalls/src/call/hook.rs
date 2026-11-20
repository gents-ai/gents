use rapidhash::{HashMapExt, RapidHashMap, RapidHashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use gents_inference::llm::message::Message;
pub use gents_inference::llm::{HookAction, ToolCallHookAction};
use gents_loop::tool::ToolDyn;
use tokio::sync::Mutex;

use crate::call::lifecycle::{InterruptDisposition, ToolCallLifecycle};
use gents_loop::output_obligation::OutputObligationCheck;
use gents_loop::truncation::TruncationLimits;
use gents_protocol::tool_call::AwaitMode;
use gents_session::session;
use gents_session::session::tool_call_state::CancelCause;
use gents_tools::mcp::meta::selected_remote_identity;

pub mod background_executions;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures;
pub mod persistence;
#[cfg(test)]
pub(crate) mod tests;

use background_executions::{
    BackgroundExecutionRegistry, BackgroundLiveOutputState, RetainedStopReceipt,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
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
    timeouts: gents_tools::surface::BackgroundTimeouts,
    limit: gents_protocol::job::BackgroundLimit,
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
                limit: Default::default(),
            }),
        }
    }

    pub fn from_config(
        tools: Vec<Box<dyn ToolDyn>>,
        config: &gents_tools::surface::BackgroundToolConfig,
    ) -> Self {
        let registry = Self::from_tools(tools, &config.allowlist);
        let inner = Arc::into_inner(registry.inner)
            .expect("a freshly built background registry has one owner");
        Self {
            inner: Arc::new(BackgroundToolRegistryInner {
                timeouts: config.timeouts.clone(),
                limit: config.limit,
                ..inner
            }),
        }
    }

    pub(crate) fn timeouts(&self) -> &gents_tools::surface::BackgroundTimeouts {
        &self.inner.timeouts
    }

    /// The Agent's resolved Tools `host.max_background_tool_calls`.
    pub(crate) fn limit(&self) -> gents_protocol::job::BackgroundLimit {
        self.inner.limit
    }

    pub(crate) fn get(&self, tool_name: &str) -> Option<Arc<dyn ToolDyn>> {
        self.inner.tools.get(tool_name).cloned()
    }

    pub(crate) fn allowlist(&self) -> Vec<String> {
        self.inner.allowlist.clone()
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

pub struct SessionState {
    session_id: Option<String>,
    current_request_id: Option<String>,
    current_request_doc_id: Option<String>,
    current_requester_did: Option<String>,
    pub request_deadline_at: Option<DateTime<Utc>>,
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

type SharedLoopTools = Arc<Vec<Box<dyn ToolDyn>>>;

#[derive(Clone)]
pub struct DefraSessionHook {
    pub node: Arc<EmbeddedNode>,
    pub actor: Option<identity::Did>,
    agent_signer: Option<gents_store::identity::ManagedAgentSigner>,
    pub node_did: String,
    truncation_limits: TruncationLimits,
    failure_policy: FailurePolicy,
    counters: Arc<HookCounters>,
    pub state: Arc<Mutex<SessionState>>,
    pub in_flight_lifecycles: Arc<Mutex<RapidHashMap<String, ToolCallLifecycle>>>,
    foreground_process_evidence: Arc<
        Mutex<RapidHashMap<String, Arc<gents_session::exec::ownership::LiveProcessEvidenceOwner>>>,
    >,
    pub accepted_tool_calls:
        Arc<Mutex<RapidHashMap<String, gents_session::request::streaming::AcceptedToolCall>>>,
    pub effect_tool_calls: Arc<Mutex<RapidHashMap<String, (ToolCallLifecycle, String)>>>,
    pub effect_lifecycles: Arc<Mutex<RapidHashMap<String, ToolCallLifecycle>>>,
    selected_loop_tools: Arc<Mutex<Option<SharedLoopTools>>>,
    background_tool_registry: BackgroundToolRegistry,
    background_executions: BackgroundExecutionRegistry,
    background_live_outputs: BackgroundLiveOutputState,
    operator_tool_root: Option<PathBuf>,
    remote_tools: Option<gents_documents::RemoteTools>,
    goal_tools_enabled: bool,
    goal_creation_enabled: bool,
    session_goals: Arc<dyn gents_session::session::message::SessionMessageGoals>,
    goal_tools: Arc<dyn persistence::goal_tools::GoalTools>,
    pub job_tools: RapidHashMap<String, crate::background::job::JobBinding>,
    /// The runtime's job driver, whose handles step 0 of a job tool runs on.
    pub job_driver: Option<Arc<crate::background::job::driver::JobDriver>>,
    output_obligation_gate: Option<Arc<dyn OutputObligationCheck>>,
    steering_input_source: Option<Arc<SteeringInputSource>>,
    inference_observation:
        Option<std::sync::Weak<gents_inference::inference_observation::InferenceObservationGuard>>,
}

struct SteeringInputSource {
    request: gents_protocol::row::AgentRequest,
    verifier: gents_session::request::admission::AgentRequestAdmissionVerifier,
    agent_identity: Arc<dyn gents_store::identity::NodeIdentity>,
    natural_finish_allowed: bool,
}

enum PolicyDecision {
    Continue,
    Terminate(String),
}

/// A provider tool call the hook adopts for dispatch: its internal and
/// provider call ids, the request and session it runs in, its tool name and
/// arguments, and the request deadline it inherits.
#[derive(Clone, Copy)]
pub(crate) struct ToolDispatch<'a> {
    pub(crate) internal_call_id: &'a str,
    pub(crate) provider_call_id: Option<&'a str>,
    pub(crate) request_id: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) tool_name: &'a str,
    pub(crate) args: &'a str,
    pub(crate) deadline_at: DateTime<Utc>,
}

impl DefraSessionHook {
    pub fn actor_access(&self) -> anyhow::Result<gents_store::config_client::ConfigAccess> {
        let actor = self
            .actor
            .clone()
            .ok_or_else(|| anyhow::anyhow!("hook has no selected agent identity"))?;
        Ok(
            gents_store::config_client::ConfigAccess::local_with_optional_agent_signer(
                self.node.clone(),
                actor,
                self.agent_signer.clone(),
            )?,
        )
    }

    pub fn with_managed_identity(
        mut self,
        identity: &dyn gents_store::identity::NodeIdentity,
    ) -> anyhow::Result<Self> {
        let actor = self
            .actor
            .clone()
            .ok_or_else(|| anyhow::anyhow!("hook has no selected agent identity"))?;
        gents_store::config_client::ConfigAccess::local_managed_agent(
            self.node.clone(),
            actor,
            identity,
        )?;
        self.agent_signer = identity.managed_agent_signer();
        Ok(self)
    }

    pub fn with_inference_observation(
        mut self,
        observation: Option<Arc<gents_inference::inference_observation::InferenceObservationGuard>>,
    ) -> Self {
        self.inference_observation = observation.as_ref().map(Arc::downgrade);
        self
    }

    pub fn with_steering_inputs(
        mut self,
        request: gents_protocol::row::AgentRequest,
        verifier: gents_session::request::admission::AgentRequestAdmissionVerifier,
        agent_identity: Arc<dyn gents_store::identity::NodeIdentity>,
        natural_finish_allowed: bool,
    ) -> Self {
        self.steering_input_source = Some(Arc::new(SteeringInputSource {
            request,
            verifier,
            agent_identity,
            natural_finish_allowed,
        }));
        self
    }
    pub async fn adopt_accepted_tool_calls(
        &self,
        calls: Vec<(String, gents_session::request::streaming::AcceptedToolCall)>,
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

    pub(crate) async fn adopt_accepted_tool_dispatch(
        &self,
        dispatch: &ToolDispatch<'_>,
        await_mode: AwaitMode,
    ) -> anyhow::Result<ToolCallLifecycle> {
        let ToolDispatch {
            internal_call_id,
            provider_call_id,
            request_id,
            session_id,
            tool_name,
            args,
            deadline_at,
        } = *dispatch;
        if let Some((lifecycle, recorded_arguments)) =
            self.effect_tool_calls.lock().await.remove(internal_call_id)
        {
            anyhow::ensure!(
                provider_call_id.is_none()
                    && lifecycle.tool_name() == tool_name
                    && lifecycle.request_id() == request_id
                    && lifecycle.session_id() == session_id
                    && recorded_arguments == args,
                "plugin effect dispatch does not match its admitted input"
            );
            return Ok(lifecycle.with_background_limit(self.background_tool_registry.limit()));
        }
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
            self.node_did.clone(),
            self.active_requester_did().await,
            accepted,
            deadline_at,
            await_mode,
        )?
        .with_selected_tool_identity(selected)
        .with_background_limit(self.background_tool_registry.limit()))
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn with_identity(
        node: Arc<EmbeddedNode>,
        node_did: &str,
        failure_policy: FailurePolicy,
    ) -> Self {
        let background_executions = BackgroundExecutionRegistry::default();
        let background_live_outputs = background_executions.live_outputs.clone();
        Self {
            node,
            actor: None,
            agent_signer: None,
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
            foreground_process_evidence: Arc::new(Mutex::new(RapidHashMap::new())),
            accepted_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_lifecycles: Arc::new(Mutex::new(RapidHashMap::new())),
            selected_loop_tools: Default::default(),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_executions,
            background_live_outputs,
            operator_tool_root: None,
            remote_tools: None,
            goal_tools_enabled: false,
            goal_creation_enabled: false,
            session_goals: Arc::new(gents_session::session::message::NoSessionMessageGoals),
            goal_tools: Arc::new(persistence::goal_tools::NoGoalTools),
            job_tools: RapidHashMap::new(),
            job_driver: None,
            output_obligation_gate: None,
            steering_input_source: None,
            inference_observation: None,
        }
    }

    pub async fn resume_with_actor_policy(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        node_did: &str,
        requester_did: Option<&str>,
        actor: ::identity::Did,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        Self::resume_with_optional_actor(
            node,
            session_id,
            node_did,
            requester_did,
            Some(actor),
            failure_policy,
        )
        .await
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub async fn resume_with_identity_policy(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        node_did: &str,
        requester_did: Option<&str>,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        Self::resume_with_optional_actor(
            node,
            session_id,
            node_did,
            requester_did,
            None,
            failure_policy,
        )
        .await
    }

    async fn resume_with_optional_actor(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        node_did: &str,
        requester_did: Option<&str>,
        actor: Option<::identity::Did>,
        failure_policy: FailurePolicy,
    ) -> anyhow::Result<Self> {
        session::require_session(&node, actor.clone(), node_did, session_id, requester_did).await?;
        let max_seq =
            session::max_sequence_as(&node, session_id, node_did, requester_did, actor.clone())
                .await?;
        let background_executions = BackgroundExecutionRegistry::default();
        let background_live_outputs = background_executions.live_outputs.clone();

        Ok(Self {
            node,
            actor,
            agent_signer: None,
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
            foreground_process_evidence: Arc::new(Mutex::new(RapidHashMap::new())),
            accepted_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_tool_calls: Arc::new(Mutex::new(RapidHashMap::new())),
            effect_lifecycles: Arc::new(Mutex::new(RapidHashMap::new())),
            selected_loop_tools: Default::default(),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_executions,
            background_live_outputs,
            operator_tool_root: None,
            remote_tools: None,
            goal_tools_enabled: false,
            goal_creation_enabled: false,
            session_goals: Arc::new(gents_session::session::message::NoSessionMessageGoals),
            goal_tools: Arc::new(persistence::goal_tools::NoGoalTools),
            job_tools: RapidHashMap::new(),
            job_driver: None,
            output_obligation_gate: None,
            steering_input_source: None,
            inference_observation: None,
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

    /// The Goals the session-message tools plan, commit and inspect with.
    pub fn with_session_goals(
        mut self,
        goals: Arc<dyn gents_session::session::message::SessionMessageGoals>,
    ) -> Self {
        self.session_goals = goals;
        self
    }

    /// The Goals plugin that answers the Goal tools.
    pub fn with_goal_tools(
        mut self,
        goal_tools: Arc<dyn persistence::goal_tools::GoalTools>,
    ) -> Self {
        self.goal_tools = goal_tools;
        self
    }

    /// Bind the Agent's job tools for their owning dispatch path.
    pub fn with_job_tools(
        mut self,
        job_tools: RapidHashMap<String, crate::background::job::JobBinding>,
    ) -> Self {
        self.job_tools = job_tools;
        self
    }

    /// Bind the runtime's job driver; a job tool is refused without it.
    pub fn with_job_driver(
        mut self,
        driver: Arc<crate::background::job::driver::JobDriver>,
    ) -> Self {
        self.job_driver = Some(driver);
        self
    }

    pub fn with_output_obligation_gate(
        mut self,
        gate: Option<Arc<dyn OutputObligationCheck>>,
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

    #[cfg(any(test, feature = "test-fixtures"))]
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

    pub async fn active_requester_did(&self) -> Option<String> {
        self.state.lock().await.current_requester_did.clone()
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub async fn active_request_doc_id(&self) -> Option<String> {
        self.state.lock().await.current_request_doc_id.clone()
    }

    async fn active_request_doc_id_for(&self, request_id: &str) -> anyhow::Result<Option<String>> {
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

    #[cfg(any(test, feature = "test-fixtures"))]
    async fn request_doc_id_for_request(&self, request_id: &str) -> anyhow::Result<Option<String>> {
        gents_session::request::binding::resolve_request_doc_id(self.node.as_ref(), request_id)
            .await
    }

    pub async fn register_stream_tool_call_identity(
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

    pub fn with_remote_tools(mut self, tools: Option<gents_documents::RemoteTools>) -> Self {
        self.remote_tools = tools;
        self
    }

    pub(crate) async fn foreground_live_output_writer(
        &self,
        internal_call_id: &str,
    ) -> gents_loop::live_output::LiveToolOutputWriter {
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

    async fn foreground_stop_confirmed(
        &self,
        lifecycle: &ToolCallLifecycle,
        receipt: RetainedStopReceipt,
    ) -> bool {
        if !gents_tools::native::is_bash_tool_name(lifecycle.tool_name()) {
            return true;
        }
        let Some(doc) = lifecycle.doc_id() else {
            return false;
        };
        let evidence = self
            .foreground_process_evidence
            .lock()
            .await
            .get(doc)
            .cloned();
        let Some(evidence) = evidence else {
            return true;
        };
        self.background_executions
            .set_stop_receipt(lifecycle.tool_call_id(), doc, receipt);
        match self
            .background_executions
            .stop_native_process(lifecycle.tool_call_id(), doc)
            .await
        {
            gents_session::exec::ProcessStopOutcome::Stopped
            | gents_session::exec::ProcessStopOutcome::AlreadyExited => true,
            gents_session::exec::ProcessStopOutcome::StillRunning => false,
            gents_session::exec::ProcessStopOutcome::NotOwned => !matches!(
                evidence.get(),
                gents_session::exec::ownership::LiveProcessEvidence::SpawnedUnverified
            ),
        }
    }

    async fn forget_foreground_evidence(&self, lifecycle: &ToolCallLifecycle) {
        if let Some(doc) = lifecycle.doc_id() {
            self.foreground_process_evidence.lock().await.remove(doc);
        }
    }

    pub async fn timeout_expired_tool_calls(&self) -> anyhow::Result<usize> {
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
                .filter_map(|id| map.remove(&id).map(|lifecycle| (id, lifecycle)))
                .collect::<Vec<_>>()
        };

        let mut count = 0;
        for (id, mut lifecycle) in lifecycles {
            if !self
                .foreground_stop_confirmed(&lifecycle, RetainedStopReceipt::Timeout)
                .await
            {
                let still_foreground = match lifecycle.doc_id() {
                    Some(doc) => self
                        .foreground_process_evidence
                        .lock()
                        .await
                        .contains_key(doc),
                    None => true,
                };
                if still_foreground {
                    self.in_flight_lifecycles.lock().await.insert(id, lifecycle);
                }
                continue;
            }
            if !self
                .background_executions
                .settle_retained_stop(&mut lifecycle)
                .await?
            {
                let _ = lifecycle.timeout().await?;
            }
            if lifecycle.is_terminal() {
                if let Some(doc) = lifecycle.doc_id() {
                    self.background_executions.retire_confirmed_stop(
                        lifecycle.tool_call_id(),
                        doc,
                        true,
                    );
                }
            }
            self.forget_foreground_evidence(&lifecycle).await;
            count += 1;
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
            map.drain().collect::<Vec<_>>()
        };

        let mut cancelled = 0;
        let mut first_error = None;
        for (id, mut lifecycle) in lifecycles {
            if lifecycle.is_running()
                && matches!(
                    lifecycle.interrupt_disposition(),
                    InterruptDisposition::Cancel
                )
                && !self
                    .foreground_stop_confirmed(
                        &lifecycle,
                        RetainedStopReceipt::Cancel(
                            CancelCause::Interrupted,
                            "request interrupted".to_owned(),
                        ),
                    )
                    .await
            {
                let still_foreground = match lifecycle.doc_id() {
                    Some(doc) => self
                        .foreground_process_evidence
                        .lock()
                        .await
                        .contains_key(doc),
                    None => true,
                };
                if still_foreground {
                    self.in_flight_lifecycles.lock().await.insert(id, lifecycle);
                }
                continue;
            }
            let applied = match lifecycle.interrupt_disposition() {
                InterruptDisposition::Cancel if lifecycle.is_running() => {
                    match self
                        .background_executions
                        .settle_retained_stop(&mut lifecycle)
                        .await
                    {
                        Ok(true) => {
                            cancelled += usize::from(lifecycle.is_cancelled());
                            Ok(())
                        }
                        Ok(false) => lifecycle
                            .cancel_during_run(CancelCause::Interrupted)
                            .await
                            .map(|won| cancelled += usize::from(won)),
                        Err(error) => Err(error),
                    }
                }
                InterruptDisposition::Cancel | InterruptDisposition::Retain => Ok(()),
            };
            if let Err(error) = applied {
                first_error.get_or_insert(error);
            } else {
                if lifecycle.is_terminal() {
                    if let Some(doc) = lifecycle.doc_id() {
                        self.background_executions.retire_confirmed_stop(
                            lifecycle.tool_call_id(),
                            doc,
                            true,
                        );
                    }
                }
                self.forget_foreground_evidence(&lifecycle).await;
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
        failure_class: gents_loop::tool_call_lifecycle::FailureClass,
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
            session::close_session_with_access(
                &self.actor_access()?,
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

impl DefraSessionHook {
    async fn ensure_steering_before_deadline(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            gents_session::request::lifecycle::RequestLifecycle::input_publication_before_deadline(
                Utc::now(),
                self.state.lock().await.request_deadline_at,
            ),
            "request deadline expired before steering intake"
        );
        Ok(())
    }
}

#[async_trait::async_trait]
impl
    gents_loop::session_hook::CanonicalSessionHook<
        gents_session::request::streaming::AcceptedToolCall,
    > for DefraSessionHook
{
    async fn adopt_accepted_tool_calls(
        &self,
        calls: Vec<(String, gents_session::request::streaming::AcceptedToolCall)>,
    ) -> Result<(), Self::Error> {
        Ok(DefraSessionHook::adopt_accepted_tool_calls(self, calls).await?)
    }
}

#[async_trait::async_trait]
impl gents_loop::session_hook::SessionHook for DefraSessionHook {
    type Error = gents_session::request::seam_error::RuntimeSeamError;
    async fn bind_tool_surface(&self, tools: SharedLoopTools) {
        *self.selected_loop_tools.lock().await = Some(tools);
    }
    fn observe_provider(&self, observation: gents_loop::session_hook::ProviderObservation) {
        if let Some(owner) = self
            .inference_observation
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        {
            owner.observe(observation);
        }
    }

    async fn run_foreground_tool(
        &self,
        name: &str,
        internal_id: &str,
        call: gents_loop::session_hook::ForegroundCall,
        context: &gents_loop::tool::ToolCallEnvelope,
    ) -> gents_loop::session_hook::ForegroundResult {
        let _activity = self
            .inference_observation
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .map(|owner| owner.begin_tool(internal_id));
        self.run_backgroundable_foreground(name, internal_id, call, context)
            .await
    }

    async fn admit_tool_effect(
        &self,
        parent_internal_id: &str,
        ordinal: u32,
        tool_name: &str,
        arguments: &str,
        context: &gents_loop::tool::ToolCallEnvelope,
    ) -> Result<String, String> {
        let parents = self.in_flight_lifecycles.lock().await;
        let parent = parents
            .get(parent_internal_id)
            .cloned()
            .ok_or_else(|| "plugin effect has no running parent".to_owned())?;
        drop(parents);
        let await_mode = if matches!(
            crate::call::hook::persistence::HookOwnedTool::of(tool_name),
            Some(crate::call::hook::persistence::HookOwnedTool::SessionMessage)
        ) {
            gents_protocol::tool_call::AwaitMode::Background
        } else {
            gents_protocol::tool_call::AwaitMode::Foreground
        };
        let child = parent
            .admit_plugin_effect_with_mode(
                ordinal,
                tool_name,
                arguments,
                await_mode,
                context.runtime().and_then(|runtime| runtime.deadline_at),
            )
            .await
            .map_err(|error| format!("cannot admit plugin effect: {error:#}"))?;
        let internal_id = child.plugin_effect_internal_id().to_owned();
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
            let mut current = ToolCallLifecycle::load_by_doc_id_as(
                self.node.clone(),
                binding.actor().clone(),
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
            current.inherit_signer(&binding)?;
            let Some(outcome) = interruption else {
                anyhow::ensure!(
                    current.is_terminal()
                        || (current.is_running()
                            && current.await_mode()
                                == gents_protocol::tool_call::AwaitMode::Background),
                    "plugin child did not durably terminalize"
                );
                gents_session::tool_call::query::result::load_tool_call_result(
                    &current.access()?,
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
                    gents_session::session::tool_call_state::CancelCause::Deadline
                }
                _ => gents_session::session::tool_call_state::CancelCause::Interrupted,
            };
            current.settle_interrupted_plugin_effect(cause).await?;
            self.in_flight_lifecycles.lock().await.remove(internal_id);
            Ok(())
        }
        .await;
        if result.is_ok() {
            self.effect_lifecycles.lock().await.remove(internal_id);
        }
        result.map_err(|error| format!("cannot settle plugin effect: {error:#}"))
    }

    async fn settle_tool_effects(
        &self,
        parent_internal_id: &str,
        stop: &gents_loop::tool_call_lifecycle::ToolOutcome,
    ) -> Result<(), String> {
        let Some(parent) = self
            .in_flight_lifecycles
            .lock()
            .await
            .get(parent_internal_id)
            .cloned()
        else {
            return Ok(());
        };
        let cause = match stop {
            gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { .. } => {
                gents_session::session::tool_call_state::CancelCause::Deadline
            }
            _ => gents_session::session::tool_call_state::CancelCause::Interrupted,
        };
        let settled = parent
            .settle_unfinished_plugin_effects(cause)
            .await
            .map_err(|error| format!("cannot settle plugin effects: {error:#}"))?;
        for internal_id in settled {
            self.effect_tool_calls.lock().await.remove(&internal_id);
            self.effect_lifecycles.lock().await.remove(&internal_id);
            self.in_flight_lifecycles.lock().await.remove(&internal_id);
        }
        Ok(())
    }

    async fn pending_steering_inputs(
        &self,
    ) -> Result<Vec<gents_loop::loop_stream::FoldedPrompt>, Self::Error> {
        let Some(source) = self.steering_input_source.as_ref() else {
            return Ok(Vec::new());
        };
        self.ensure_steering_before_deadline().await?;
        Ok(gents_session::request::lifecycle::queue::steering_inputs(
            &self.node,
            &source.request,
            &source.verifier,
            Some(source.agent_identity.as_ref()),
        )
        .await?)
    }

    async fn finish_steering_snapshot(
        &self,
    ) -> Result<Option<gents_loop::loop_stream::SteeringSnapshot>, Self::Error> {
        let Some(source) = self
            .steering_input_source
            .as_ref()
            .filter(|source| source.natural_finish_allowed)
        else {
            return Ok(None);
        };
        self.ensure_steering_before_deadline().await?;
        Ok(Some(
            gents_session::request::lifecycle::queue::steering_snapshot(
                &self.node,
                &source.request,
                &source.verifier,
                Some(source.agent_identity.as_ref()),
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
        context: &gents_loop::tool::ToolCallEnvelope,
    ) -> ToolCallHookAction {
        DefraSessionHook::on_tool_call(
            self,
            tool_name,
            tool_call_id,
            internal_call_id,
            args,
            context,
        )
        .await
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

#[cfg(test)]
mod conformance {
    use super::FailurePolicy;

    #[test]
    fn failure_policies_are_the_lean_persistence_failure_policies() {
        gents_lean_vocab::families::persistence_state_failure_policy_vocabulary::replay(&[
            FailurePolicy::FailOpen,
            FailurePolicy::FailClosed,
        ]);
    }
}
