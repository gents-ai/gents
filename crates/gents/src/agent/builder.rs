use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use crate::llm::tool::ToolDyn;
use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;

use super::{assemble_node_and_agents, runtime, AgentBuildError, Gents, ProcessLifecycleObserver};
use crate::admission::BackendAdmissionConfig;
use crate::agent::completion_retry::CompletionRetryProfileFields;
#[cfg(test)]
use crate::backend_provider::BackendProviderKind;
use crate::backend_registry::lookup_backend;
use crate::compaction::CompactionStrategy;
use crate::config::{
    ResolvedAgent, SamplingConfig, DEFAULT_COMPACTION_THRESHOLD, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS,
    DEFAULT_MODEL_NAME, DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS, DEFAULT_STREAM_BATCH_MS,
    DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
};
use crate::health_checker::HealthCheckerOptions;
use crate::hook::{BackgroundExecutionRegistry, FailurePolicy};
use crate::identity::{NodeIdentity, RuntimeNode};
use crate::mcp_pool::McpPool;
use crate::retry::RetryPolicy;
use crate::tool_surface::{
    AgentToolSurfaceConfig, BashMode, CustomToolFactory, FileToolMode, ResolvedToolSelection,
    ToolCeiling,
};

#[cfg(test)]
const TEST_DEFAULT_BACKEND_ENDPOINT: &str = "http://localhost:8000/v1";

#[derive(Default)]
pub struct GentsBuilder {
    node: Option<Arc<EmbeddedNode>>,
    identity: Option<Arc<dyn NodeIdentity>>,
    default_agent_id: Option<String>,
    tool_ceiling: ToolCeiling,
    mcp_pool: McpPool,
    local_hostname: Option<String>,
    local_subnet: Option<String>,
    retry_policy: RetryPolicy,
    hook_failure_policy: FailurePolicy,
    health_checker_options: HealthCheckerOptions,
    process_state_observer: Option<Arc<dyn ProcessLifecycleObserver>>,
    rendered_request_capture_factory:
        Option<crate::rendered_request::RenderedRequestCaptureFactory>,
    agents: Vec<PendingAgent>,
    /// This runtime's measured probe health (#640), consulted alongside the
    /// document when admitting an agent_config's backend. Empty by default —
    /// build paths that run before the prober has an opinion (startup,
    /// tests) never veto on measured health, matching
    /// `BackendAdmissionConfig::measured_unhealthy`'s own default.
    backend_health: crate::backend_health::BackendHealthMap,
    plugin_home: Option<std::path::PathBuf>,
}

impl GentsBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn node(mut self, node: Arc<EmbeddedNode>) -> Self {
        self.node = Some(node);
        self
    }

    pub fn identity(mut self, identity: Arc<dyn NodeIdentity>) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Test-only: seed the measured probe health this build should veto on,
    /// exercising the same `BackendAdmissionConfig` path the runtime
    /// consults after startup.
    #[cfg(test)]
    pub(crate) fn backend_health(
        mut self,
        backend_health: crate::backend_health::BackendHealthMap,
    ) -> Self {
        self.backend_health = backend_health;
        self
    }

    pub fn default_agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.default_agent_id = Some(agent_id.into());
        self
    }

    pub fn tool_ceiling(mut self, tool_ceiling: ToolCeiling) -> Self {
        self.tool_ceiling = tool_ceiling;
        self
    }

    /// The gents home whose installed plugins this runtime's tools can call.
    pub fn plugin_home(mut self, home: impl Into<std::path::PathBuf>) -> Self {
        self.plugin_home = Some(home.into());
        self
    }

    pub fn mcp_pool(mut self, mcp_pool: McpPool) -> Self {
        self.mcp_pool = mcp_pool;
        self
    }

    pub fn local_hostname(mut self, local_hostname: impl Into<String>) -> Self {
        self.local_hostname = Some(local_hostname.into());
        self
    }

    pub fn local_subnet(mut self, local_subnet: impl Into<String>) -> Self {
        self.local_subnet = Some(local_subnet.into());
        self
    }

    pub fn retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    pub fn hook_failure_policy(mut self, hook_failure_policy: FailurePolicy) -> Self {
        self.hook_failure_policy = hook_failure_policy;
        self
    }

    pub fn health_checker_options(mut self, options: HealthCheckerOptions) -> Self {
        self.health_checker_options = options;
        self
    }

    pub fn process_state_observer(mut self, observer: Arc<dyn ProcessLifecycleObserver>) -> Self {
        self.process_state_observer = Some(observer);
        self
    }

    /// Install a fail-closed capture sink for end-to-end fault-injection tests.
    ///
    /// This deliberately exposes only failure, not arbitrary sink replacement:
    /// callers must not be able to acknowledge a provider request without
    /// first making its rendered input durable.
    #[doc(hidden)]
    pub fn fail_rendered_request_capture_for_test(mut self, message: impl Into<String>) -> Self {
        let message = Arc::new(message.into());
        self.rendered_request_capture_factory = Some(Arc::new(move |_context| {
            let message = Arc::clone(&message);
            Arc::new(move |_rendered| {
                let message = Arc::clone(&message);
                Box::pin(async move { Err(anyhow!(message.as_str().to_owned())) })
            })
        }));
        self
    }

    pub fn agent(self, name: impl Into<String>) -> AgentBuilder {
        AgentBuilder {
            agent: self,
            pending_agent: PendingAgent::new(name),
        }
    }

    pub async fn build(self) -> Result<Gents> {
        let node = self
            .node
            .ok_or_else(|| anyhow!("Gents builder is missing node"))?;
        let identity = self
            .identity
            .ok_or_else(|| anyhow!("Gents builder is missing identity"))?;
        if node.node_identity_did().is_none() {
            anyhow::bail!(
                "Gents runtime requires an EmbeddedNode configured with a node signing DID"
            );
        }
        if self.agents.is_empty() {
            anyhow::bail!("Gents builder requires at least one agent");
        }

        let default_agent_id = self
            .default_agent_id
            .clone()
            .unwrap_or_else(|| self.agents[0].name.clone());
        let agent_names = self
            .agents
            .iter()
            .map(|agent_config| agent_config.name.clone())
            .collect::<Vec<_>>();
        if !agent_names.iter().any(|name| name == &default_agent_id) {
            anyhow::bail!(
                "default agent {} is not present in builder agents",
                default_agent_id
            );
        }
        let duplicates = find_duplicates(&agent_names);
        if !duplicates.is_empty() {
            anyhow::bail!(
                "duplicate agent names in builder: {}",
                duplicates.into_iter().collect::<Vec<_>>().join(", ")
            );
        }

        let mut agent_factories: Vec<
            Box<
                dyn FnOnce(Arc<RuntimeNode>) -> std::result::Result<ResolvedAgent, AgentBuildError>
                    + Send,
            >,
        > = Vec::with_capacity(self.agents.len());
        for agent_config in self.agents {
            let factory = agent_config
                .into_factory(
                    node.as_ref(),
                    identity.did(),
                    &self.tool_ceiling,
                    &self.backend_health,
                )
                .await?;
            agent_factories.push(factory);
        }

        let node_data = RuntimeNode {
            node_did: identity.did().to_string(),
            identity: identity.clone(),
            default_agent_id: default_agent_id.clone(),
            display_name: None,
            enabled: true,
        };

        let (principal, agent_results) = assemble_node_and_agents(node_data, agent_factories);

        let mut agents = Vec::with_capacity(agent_results.len());
        for result in agent_results {
            let agent_arc = result
                .map_err(|e| anyhow::anyhow!("agent '{}' build failed: {}", e.agent_id, e.error))?;
            agents.push(agent_arc);
        }
        agents.sort_by(|left, right| {
            let left_is_default = left.agent_id == default_agent_id;
            let right_is_default = right.agent_id == default_agent_id;
            right_is_default
                .cmp(&left_is_default)
                .then_with(|| left.agent_id.cmp(&right.agent_id))
        });

        let capture_node = node.clone();
        let plugin_node = node.clone();

        Ok(Gents {
            node,
            runtime_node: principal,
            agents,
            unavailable_agents: Default::default(),
            document_runtime_context: None,
            mcp_pool: self.mcp_pool,
            local_hostname: self
                .local_hostname
                .unwrap_or_else(runtime::default_hostname),
            local_subnet: self.local_subnet,
            retry_policy: self.retry_policy,
            hook_failure_policy: self.hook_failure_policy,
            background_execution_registry: BackgroundExecutionRegistry::default(),
            health_checker_options: self.health_checker_options,
            backend_prober_options: crate::backend_health::BackendProberOptions::default(),
            backend_health: self.backend_health,
            process_state_observer: self.process_state_observer,
            runtime_snapshot_observer: None,
            startup_build_failure_observer: None,
            startup_readiness: Default::default(),
            #[cfg(test)]
            router_dispatch_probe: None,
            // Same default as `Gents::from_default_agent_documents`: every
            // provider request must pass through the durable DefraDB sink.
            rendered_request_capture_factory: Some(
                self.rendered_request_capture_factory.unwrap_or_else(|| {
                    crate::rendered_request::defra_rendered_request_capture_factory(capture_node)
                }),
            ),
            manual_trigger_handle: Arc::new(tokio::sync::OnceCell::new()),
            operator_tool_root: self.tool_ceiling.root().map(std::path::PathBuf::from),
            plugins: Arc::new(crate::agent::plugin_executor(
                self.plugin_home,
                crate::plugin::model_calls::AccessModels(
                    crate::config_client::ConfigAccess::Local(plugin_node),
                ),
            )),
        })
    }
}

pub struct AgentBuilder {
    agent: GentsBuilder,
    pending_agent: PendingAgent,
}

impl AgentBuilder {
    pub fn backend_id(mut self, backend_id: impl Into<String>) -> Self {
        self.pending_agent.backend_id = Some(backend_id.into());
        self
    }

    pub fn model_name(mut self, model_name: impl Into<String>) -> Self {
        self.pending_agent.model_name = model_name.into();
        self
    }

    pub fn sampling(mut self, sampling: SamplingConfig) -> Self {
        self.pending_agent.sampling = sampling;
        self
    }

    pub fn system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.pending_agent.system_prompt = system_prompt.into();
        self
    }

    pub fn context_window(mut self, context_window: usize) -> Self {
        self.pending_agent.context_window = context_window;
        self
    }

    pub fn max_output_tokens(mut self, max_output_tokens: usize) -> Self {
        self.pending_agent.max_output_tokens = max_output_tokens;
        self
    }

    pub fn max_turns(mut self, max_turns: usize) -> Self {
        self.pending_agent.max_turns = max_turns;
        self.pending_agent.max_turns_explicit = true;
        self
    }

    pub fn enable_file_tools(mut self, mode: FileToolMode) -> Self {
        self.pending_agent.tool_selection.file_tools = mode;
        self
    }

    pub fn enable_bash(mut self, mode: BashMode) -> Self {
        self.pending_agent.tool_selection.bash = mode;
        self
    }

    pub fn cli_tools<I, S>(mut self, cli_tool_names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.pending_agent.tool_selection.cli_tool_names =
            cli_tool_names.into_iter().map(Into::into).collect();
        self
    }

    pub fn enable_meta_tools(mut self, enable_meta_tools: bool) -> Self {
        self.pending_agent.tool_selection.enable_meta_tools = enable_meta_tools;
        self
    }

    pub fn enable_goal_tools(mut self, enable_goal_tools: bool) -> Self {
        self.pending_agent.tool_selection.enable_goal_tools = enable_goal_tools;
        self
    }

    pub fn enable_goal_creation(mut self, enable_goal_creation: bool) -> Self {
        self.pending_agent.tool_selection.enable_goal_creation = enable_goal_creation;
        self
    }

    pub fn enable_defra_query(mut self, enable_defra_query: bool) -> Self {
        self.pending_agent.tool_selection.enable_defra_query = enable_defra_query;
        self
    }

    pub fn enable_self_config(mut self, enable_self_config: bool) -> Self {
        self.pending_agent.tool_selection.enable_self_config = enable_self_config;
        self
    }

    pub fn self_config_categories<I, S>(mut self, categories: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.pending_agent.tool_selection.self_config_categories =
            Some(categories.into_iter().map(Into::into).collect());
        self
    }

    pub fn self_config_no_lockout(mut self, no_lockout: bool) -> Self {
        self.pending_agent.tool_selection.self_config_no_lockout = no_lockout;
        self
    }

    pub fn self_config_preview(mut self, preview: bool) -> Self {
        self.pending_agent.tool_selection.self_config_preview = preview;
        self
    }

    pub fn enable_context_budget(mut self, enable_context_budget: bool) -> Self {
        self.pending_agent.tool_selection.enable_context_budget = enable_context_budget;
        self
    }

    pub fn enable_memory(mut self, enable_memory: bool) -> Self {
        self.pending_agent.tool_selection.enable_memory = enable_memory;
        self
    }

    pub fn enable_session_history_tool(mut self, enable_session_history_tool: bool) -> Self {
        self.pending_agent
            .tool_selection
            .enable_session_history_tool = enable_session_history_tool;
        self
    }

    pub fn defra_query_collections<I, S>(mut self, collections: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.pending_agent.tool_selection.defra_query_collections =
            collections.into_iter().map(Into::into).collect();
        self
    }

    pub fn allowed_mcp_service_ids<I, S>(mut self, service_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.pending_agent.tool_selection.allowed_mcp_service_ids =
            service_ids.into_iter().map(Into::into).collect();
        self
    }

    pub fn custom_tool<T>(mut self, tool: T) -> Self
    where
        T: ToolDyn + Clone + Send + Sync + 'static,
    {
        self.pending_agent
            .custom_tools
            .push(CustomToolFactory::from_tool(tool));
        self
    }

    pub fn custom_tool_factory(mut self, tool: CustomToolFactory) -> Self {
        self.pending_agent.custom_tools.push(tool);
        self
    }

    pub fn compaction_threshold(mut self, compaction_threshold: f64) -> Self {
        self.pending_agent.compaction_threshold = compaction_threshold;
        self
    }

    pub fn compaction_strategy(mut self, compaction_strategy: CompactionStrategy) -> Self {
        self.pending_agent.compaction_strategy = compaction_strategy;
        self
    }

    pub fn stream_batch_ms(mut self, stream_batch_ms: u64) -> Self {
        self.pending_agent.stream_batch_ms = stream_batch_ms;
        self
    }

    pub fn stream_liveness_timeout_secs(mut self, stream_liveness_timeout_secs: u64) -> Self {
        self.pending_agent.stream_liveness_timeout =
            Duration::from_secs(stream_liveness_timeout_secs);
        self
    }

    pub fn provider_idle_timeout_secs(mut self, provider_idle_timeout_secs: u64) -> Self {
        self.pending_agent.provider_idle_timeout = Duration::from_secs(provider_idle_timeout_secs);
        self
    }

    pub fn deadline_duration_secs(mut self, deadline_duration_secs: u64) -> Self {
        self.pending_agent.deadline_duration = Duration::from_secs(deadline_duration_secs);
        self
    }

    pub fn done(mut self) -> GentsBuilder {
        self.agent.agents.push(self.pending_agent);
        self.agent
    }
}

fn find_duplicates(values: &[String]) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();
    for value in values {
        if !seen.insert(value.clone()) {
            duplicates.insert(value.clone());
        }
    }
    duplicates
}

#[derive(Clone)]
pub(crate) struct PendingAgent {
    name: String,
    backend_id: Option<String>,
    #[cfg(test)]
    backend_endpoint: String,
    model_name: String,
    context_window: usize,
    max_output_tokens: usize,
    max_turns: usize,
    max_turns_explicit: bool,
    system_prompt: String,
    tool_selection: ResolvedToolSelection,
    custom_tools: Vec<CustomToolFactory>,
    compaction_threshold: f64,
    compaction_strategy: CompactionStrategy,
    stream_batch_ms: u64,
    stream_liveness_timeout: Duration,
    provider_idle_timeout: Duration,
    deadline_duration: Duration,
    sampling: SamplingConfig,
    skills: Vec<crate::skills::Skill>,
}

impl PendingAgent {
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            backend_id: None,
            #[cfg(test)]
            backend_endpoint: TEST_DEFAULT_BACKEND_ENDPOINT.to_string(),
            model_name: DEFAULT_MODEL_NAME.to_string(),
            context_window: DEFAULT_CONTEXT_WINDOW,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            max_turns: DEFAULT_MAX_TURNS,
            max_turns_explicit: false,
            system_prompt: String::new(),
            tool_selection: ResolvedToolSelection::default(),
            custom_tools: Vec::new(),
            compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
            compaction_strategy: CompactionStrategy::StripThenSummarize,
            stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
            stream_liveness_timeout: Duration::from_secs(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS),
            provider_idle_timeout: Duration::from_secs(DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS),
            deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
            sampling: SamplingConfig::default(),
            skills: Vec::new(),
        }
    }

    async fn into_factory(
        self,
        node: &EmbeddedNode,
        node_did: &str,
        tool_ceiling: &ToolCeiling,
        backend_health: &crate::backend_health::BackendHealthMap,
    ) -> Result<
        Box<
            dyn FnOnce(Arc<RuntimeNode>) -> std::result::Result<ResolvedAgent, AgentBuildError>
                + Send,
        >,
    > {
        let backend_id = self
            .backend_id
            .as_deref()
            .ok_or_else(|| anyhow!("agent '{}' is missing backend_id", self.name))?
            .to_string();
        let backend = lookup_backend(node, node_did, &backend_id)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "agent '{}' references missing backend {}",
                    self.name,
                    backend_id
                )
            })?;
        // `BackendAdmissionConfig::is_available` is the single owner of the
        // enabled/probe_status/measured_unhealthy comparison (#1332); apply
        // this builder's measured health the same way the reconciler does
        // before gating on it.
        let observation =
            crate::backend_registry::lookup_backend_observation(node, node_did, &backend_id)
                .await?
                .ok_or_else(|| anyhow!("backend {} disappeared during assembly", backend_id))?;
        let admission_config = BackendAdmissionConfig::from_backend(&backend, &observation)?
            .with_measured_unhealthy(backend_health.measured_blocks_routing(&backend_id).await);
        if !admission_config.is_available() {
            anyhow::bail!(
                "agent '{}' backend {} is unavailable (enabled={} probe_status={} measured_unhealthy={})",
                self.name,
                backend_id,
                backend.enabled,
                admission_config.probe_status,
                admission_config.measured_unhealthy,
            );
        }

        let agent_name = self.name.clone();
        let backend_fields = backend.backend_fields();
        let tool_ceiling = tool_ceiling.clone();

        Ok(Box::new(move |principal| {
            self.build_with_resolved_backend(principal, backend_fields, &tool_ceiling)
                .map_err(|error| AgentBuildError {
                    agent_id: agent_name,
                    error,
                })
        }))
    }

    fn build_with_resolved_backend(
        self,
        principal: Arc<RuntimeNode>,
        backend_fields: crate::backend_registry::BackendFields,
        tool_ceiling: &ToolCeiling,
    ) -> Result<ResolvedAgent> {
        let agent_name = self.name.clone();
        self.sampling.validate_for_provider(
            backend_fields.backend_provider_kind,
            backend_fields.openai_wire_api,
        )?;

        let compaction = crate::document_config::CompactionConfig {
            compaction_id: self.name.clone(),
            node_did: principal.node_did.clone(),
            display_name: None,
            strategy: self.compaction_strategy,
            threshold: Some(self.compaction_threshold),
            keep_recent_tokens: None,
            tool_result_max_chars: None,
            summary_max_output_tokens: None,
            summary_file_list_max: None,
            inference_profile_id: None,
            tags: Vec::new(),
        };
        compaction.validate()?;
        Ok(ResolvedAgent {
            agent_id: self.name,
            node: principal,
            backend_id: backend_fields.backend_id,
            backend_provider_kind: backend_fields.backend_provider_kind,
            openai_wire_api: backend_fields.openai_wire_api,
            backend_endpoint: backend_fields.backend_endpoint,
            backend_auth: backend_fields.backend_auth,
            model_name: self.model_name,
            resolved_reasoning_efforts: None,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            max_turns: self.max_turns,
            max_turns_provenance: if self.max_turns_explicit {
                crate::config::MaxTurnsProvenance::BuilderOverride
            } else {
                crate::config::MaxTurnsProvenance::Default
            },
            system_prompt: self.system_prompt,
            tools: AgentToolSurfaceConfig::from_selection(
                &agent_name,
                self.tool_selection,
                tool_ceiling,
                self.custom_tools,
            )?,
            compaction: Some(compaction),
            compaction_inference: None,
            max_total_tokens: None,
            stream_batch_ms: self.stream_batch_ms,
            stream_liveness_timeout: self.stream_liveness_timeout,
            provider_idle_timeout: self.provider_idle_timeout,
            deadline_duration: self.deadline_duration,
            completion_retry: CompletionRetryProfileFields::default(),
            sampling: self.sampling,
            skills: self.skills,
        })
    }
}

#[cfg(test)]
impl PendingAgent {
    pub(crate) fn build_with_identity_for_test<I>(self, identity: I) -> ResolvedAgent
    where
        I: NodeIdentity + 'static,
    {
        let backend_id = self.backend_id.clone();
        let backend_endpoint = self.backend_endpoint.clone();
        let agent_name = self.name.clone();
        let identity: Arc<dyn NodeIdentity> = Arc::new(identity);
        let principal = Arc::new(RuntimeNode {
            node_did: identity.did().to_string(),
            identity,
            default_agent_id: agent_name.clone(),
            display_name: None,
            enabled: true,
        });
        self.build_with_resolved_backend(
            principal,
            crate::backend_registry::BackendFields {
                backend_id,
                backend_provider_kind: BackendProviderKind::OpenAiCompatible,
                openai_wire_api: crate::OpenAiWireApi::effective_for_provider(
                    BackendProviderKind::OpenAiCompatible,
                    None,
                ),
                backend_endpoint,
                backend_auth: crate::document_config::BackendAuth::Unauthenticated,
            },
            &ToolCeiling::meta_only(),
        )
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MaxTurnsProvenance, DEFAULT_MAX_TURNS};
    use crate::identity::KeyIdentity;

    fn test_identity(name: &str) -> KeyIdentity {
        let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
        KeyIdentity::load_or_create(path, None).unwrap()
    }

    #[test]
    fn builder_max_turns_resolves_to_a_programmatic_provenance() {
        let agent_config = GentsBuilder::default()
            .agent("general")
            .max_turns(40)
            .pending_agent
            .build_with_identity_for_test(test_identity("builder-max-turns-explicit"));

        assert_eq!(agent_config.max_turns, 40);
        assert_eq!(
            agent_config.max_turns_provenance,
            MaxTurnsProvenance::BuilderOverride
        );
        let message = agent_config.max_turns_provenance.describe();
        assert!(
            message.contains("AgentBuilder::max_turns"),
            "a builder-configured limit must name the builder: {message}"
        );
        assert!(
            message.contains("no InferenceExecution document"),
            "a builder-configured limit has no owning document to edit: {message}"
        );
    }

    #[test]
    fn builder_without_max_turns_resolves_to_the_built_in_default() {
        let agent_config = GentsBuilder::default()
            .agent("general")
            .pending_agent
            .build_with_identity_for_test(test_identity("builder-max-turns-default"));

        assert_eq!(agent_config.max_turns, DEFAULT_MAX_TURNS);
        assert_eq!(
            agent_config.max_turns_provenance,
            MaxTurnsProvenance::Default
        );
        // This agent has no InferenceExecution document, so advice naming
        // only that document would not raise its limit.
        let message = agent_config.max_turns_provenance.describe();
        assert!(
            message.contains("AgentBuilder::max_turns"),
            "a default limit on a built agent must name the builder knob: {message}"
        );
    }
}
