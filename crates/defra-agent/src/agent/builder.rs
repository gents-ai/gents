// Soft-cap justified: single builder type with a linear fluent API; splitting
// by concern (tool config, behavior config, identity) would create artificial
// seams in a pattern that is intentionally read top-to-bottom.
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use rig::tool::ToolDyn;

use super::{
    assemble_principal_and_behaviors, runtime, BehaviorBuildError, DefraAgent,
    ProcessLifecycleObserver,
};
use crate::admission::BackendAdmissionConfig;
use crate::backend_provider::BackendProviderKind;
use crate::backend_registry::lookup_backend;
use crate::compaction::CompactionStrategy;
use crate::config::{
    AgentBehavior, SamplingConfig, DEFAULT_COMPACTION_THRESHOLD, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS,
    DEFAULT_MODEL_NAME, DEFAULT_STREAM_BATCH_MS,
};
use crate::hook::FailurePolicy;
use crate::identity::{AgentIdentity, AgentPrincipal};
use crate::mcp_pool::McpPool;
use crate::retry::RetryPolicy;
use crate::tool_surface::{
    BashMode, BehaviorToolConfig, CustomToolFactory, FileToolMode, ToolCeiling, ToolSelection,
};

#[cfg(test)]
const TEST_DEFAULT_BACKEND_ENDPOINT: &str = "http://localhost:8000/v1";

#[derive(Default)]
pub struct DefraAgentBuilder {
    node: Option<Arc<EmbeddedNode>>,
    identity: Option<Arc<dyn AgentIdentity>>,
    default_behavior_id: Option<String>,
    tool_ceiling: ToolCeiling,
    mcp_pool: McpPool,
    local_hostname: Option<String>,
    local_subnet: Option<String>,
    retry_policy: RetryPolicy,
    hook_failure_policy: FailurePolicy,
    process_state_observer: Option<Arc<dyn ProcessLifecycleObserver>>,
    behaviors: Vec<PendingAgentBehavior>,
}

impl DefraAgentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn node(mut self, node: Arc<EmbeddedNode>) -> Self {
        self.node = Some(node);
        self
    }

    pub fn identity(mut self, identity: Arc<dyn AgentIdentity>) -> Self {
        self.identity = Some(identity);
        self
    }

    pub fn default_behavior_id(mut self, behavior_id: impl Into<String>) -> Self {
        self.default_behavior_id = Some(behavior_id.into());
        self
    }

    pub fn tool_ceiling(mut self, tool_ceiling: ToolCeiling) -> Self {
        self.tool_ceiling = tool_ceiling;
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

    pub fn process_state_observer(mut self, observer: Arc<dyn ProcessLifecycleObserver>) -> Self {
        self.process_state_observer = Some(observer);
        self
    }

    pub fn behavior(self, name: impl Into<String>) -> BehaviorBuilder {
        BehaviorBuilder {
            agent: self,
            behavior: PendingAgentBehavior::new(name),
        }
    }

    pub async fn build(self) -> Result<DefraAgent> {
        let node = self
            .node
            .ok_or_else(|| anyhow!("DefraAgent builder is missing node"))?;
        let identity = self
            .identity
            .ok_or_else(|| anyhow!("DefraAgent builder is missing identity"))?;
        if self.behaviors.is_empty() {
            anyhow::bail!("DefraAgent builder requires at least one behavior");
        }

        let default_behavior_id = self
            .default_behavior_id
            .clone()
            .unwrap_or_else(|| self.behaviors[0].name.clone());
        let behavior_names = self
            .behaviors
            .iter()
            .map(|behavior| behavior.name.clone())
            .collect::<Vec<_>>();
        if !behavior_names
            .iter()
            .any(|name| name == &default_behavior_id)
        {
            anyhow::bail!(
                "default behavior {} is not present in builder behaviors",
                default_behavior_id
            );
        }
        let duplicates = find_duplicates(&behavior_names);
        if !duplicates.is_empty() {
            anyhow::bail!(
                "duplicate behavior names in builder: {}",
                duplicates.into_iter().collect::<Vec<_>>().join(", ")
            );
        }

        // Async-resolve every behavior into a sync factory closure that
        // accepts `Arc<AgentPrincipal>`. The actual `Arc::new(AgentPrincipal
        // { ... })` is constructed exactly once inside
        // `assemble_principal_and_behaviors` below.
        let mut behavior_factories: Vec<
            Box<
                dyn FnOnce(
                        Arc<AgentPrincipal>,
                    )
                        -> std::result::Result<AgentBehavior, BehaviorBuildError>
                    + Send,
            >,
        > = Vec::with_capacity(self.behaviors.len());
        for behavior in self.behaviors {
            let factory = behavior
                .into_factory(node.as_ref(), &self.tool_ceiling)
                .await?;
            behavior_factories.push(factory);
        }

        let principal_data = AgentPrincipal {
            agent_did: identity.did().to_string(),
            identity: identity.clone(),
            default_behavior_id: default_behavior_id.clone(),
            display_name: None,
            enabled: true,
        };

        let (principal, behavior_results) =
            assemble_principal_and_behaviors(principal_data, behavior_factories);

        let mut behaviors = Vec::with_capacity(behavior_results.len());
        for result in behavior_results {
            let behavior_arc = result.map_err(|e| {
                anyhow::anyhow!("behavior '{}' build failed: {}", e.behavior_id, e.error)
            })?;
            behaviors.push(behavior_arc);
        }
        behaviors.sort_by(|left, right| {
            let left_is_default = left.behavior_id == default_behavior_id;
            let right_is_default = right.behavior_id == default_behavior_id;
            right_is_default
                .cmp(&left_is_default)
                .then_with(|| left.behavior_id.cmp(&right.behavior_id))
        });

        Ok(DefraAgent {
            node,
            principal,
            behaviors,
            unavailable_behaviors: Default::default(),
            document_runtime_context: None,
            mcp_pool: self.mcp_pool,
            local_hostname: self
                .local_hostname
                .unwrap_or_else(runtime::default_hostname),
            local_subnet: self.local_subnet,
            retry_policy: self.retry_policy,
            hook_failure_policy: self.hook_failure_policy,
            process_state_observer: self.process_state_observer,
            manual_trigger_handle: Arc::new(tokio::sync::OnceCell::new()),
        })
    }
}

pub struct BehaviorBuilder {
    agent: DefraAgentBuilder,
    behavior: PendingAgentBehavior,
}

impl BehaviorBuilder {
    pub fn backend_id(mut self, backend_id: impl Into<String>) -> Self {
        self.behavior.backend_id = Some(backend_id.into());
        self
    }

    pub fn model_name(mut self, model_name: impl Into<String>) -> Self {
        self.behavior.model_name = model_name.into();
        self
    }

    pub fn system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.behavior.system_prompt = system_prompt.into();
        self
    }

    pub fn context_window(mut self, context_window: usize) -> Self {
        self.behavior.context_window = context_window;
        self
    }

    pub fn max_output_tokens(mut self, max_output_tokens: usize) -> Self {
        self.behavior.max_output_tokens = max_output_tokens;
        self
    }

    pub fn max_turns(mut self, max_turns: usize) -> Self {
        self.behavior.max_turns = max_turns;
        self
    }

    pub fn enable_file_tools(mut self, mode: FileToolMode) -> Self {
        self.behavior.tool_selection.file_tools = mode;
        self
    }

    pub fn enable_bash(mut self, mode: BashMode) -> Self {
        self.behavior.tool_selection.bash = mode;
        self
    }

    pub fn cli_tools<I, S>(mut self, cli_tool_names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.behavior.tool_selection.cli_tool_names =
            cli_tool_names.into_iter().map(Into::into).collect();
        self
    }

    pub fn enable_meta_tools(mut self, enable_meta_tools: bool) -> Self {
        self.behavior.tool_selection.enable_meta_tools = enable_meta_tools;
        self
    }

    pub fn allowed_mcp_service_ids<I, S>(mut self, service_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.behavior.tool_selection.allowed_mcp_service_ids =
            service_ids.into_iter().map(Into::into).collect();
        self
    }

    pub fn delegate_to<I, S>(mut self, delegate_to: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.behavior.tool_selection.delegate_to =
            delegate_to.into_iter().map(Into::into).collect();
        self
    }

    pub fn custom_tool<T>(mut self, tool: T) -> Self
    where
        T: ToolDyn + Clone + Send + Sync + 'static,
    {
        self.behavior
            .custom_tools
            .push(CustomToolFactory::from_tool(tool));
        self
    }

    pub fn custom_tool_factory(mut self, tool: CustomToolFactory) -> Self {
        self.behavior.custom_tools.push(tool);
        self
    }

    pub fn compaction_threshold(mut self, compaction_threshold: f64) -> Self {
        self.behavior.compaction_threshold = compaction_threshold;
        self
    }

    pub fn compaction_strategy(mut self, compaction_strategy: CompactionStrategy) -> Self {
        self.behavior.compaction_strategy = compaction_strategy;
        self
    }

    pub fn stream_batch_ms(mut self, stream_batch_ms: u64) -> Self {
        self.behavior.stream_batch_ms = stream_batch_ms;
        self
    }

    pub fn deadline_duration_secs(mut self, deadline_duration_secs: u64) -> Self {
        self.behavior.deadline_duration = Duration::from_secs(deadline_duration_secs);
        self
    }

    pub fn done(mut self) -> DefraAgentBuilder {
        self.agent.behaviors.push(self.behavior);
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
pub(crate) struct PendingAgentBehavior {
    name: String,
    backend_id: Option<String>,
    #[cfg(test)]
    backend_endpoint: String,
    model_name: String,
    context_window: usize,
    max_output_tokens: usize,
    max_turns: usize,
    system_prompt: String,
    tool_selection: ToolSelection,
    custom_tools: Vec<CustomToolFactory>,
    compaction_threshold: f64,
    compaction_strategy: CompactionStrategy,
    stream_batch_ms: u64,
    deadline_duration: Duration,
    sampling: SamplingConfig,
}

impl PendingAgentBehavior {
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
            system_prompt: String::new(),
            tool_selection: ToolSelection::default(),
            custom_tools: Vec::new(),
            compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
            compaction_strategy: CompactionStrategy::StripThenSummarize,
            stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
            deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
            sampling: SamplingConfig::default(),
        }
    }

    /// Async phase: resolve the backend and validate it. Returns a sync
    /// factory closure that accepts `Arc<AgentPrincipal>` and produces
    /// the fully-built `AgentBehavior`.
    ///
    /// This split lets `DefraAgentBuilder::build` collect all factory
    /// closures before calling `assemble_principal_and_behaviors`, so
    /// that the single `Arc::new(AgentPrincipal { ... })` lives
    /// exclusively in the helper (the load-bearing site fenced by the
    /// loader-dedup proptest).
    async fn into_factory(
        self,
        node: &EmbeddedNode,
        tool_ceiling: &ToolCeiling,
    ) -> Result<
        Box<
            dyn FnOnce(
                    Arc<AgentPrincipal>,
                ) -> std::result::Result<AgentBehavior, BehaviorBuildError>
                + Send,
        >,
    > {
        let backend_id = self
            .backend_id
            .as_deref()
            .ok_or_else(|| anyhow!("behavior '{}' is missing backend_id", self.name))?
            .to_string();
        let backend = lookup_backend(node, &backend_id).await?.ok_or_else(|| {
            anyhow!(
                "behavior '{}' references missing backend {}",
                self.name,
                backend_id
            )
        })?;
        if !backend.is_available() {
            anyhow::bail!(
                "behavior '{}' backend {} is unavailable (enabled={} probe_status={})",
                self.name,
                backend_id,
                backend.enabled,
                backend.probe_status
            );
        }
        BackendAdmissionConfig::from_backend(&backend)?;

        let behavior_name = self.name.clone();
        let resolved_backend_id = Some(backend.backend_id);
        let provider_kind = backend.provider_kind;
        let endpoint = backend.endpoint;
        let api_key = backend.api_key;
        let api_key_env_var = backend.api_key_env_var;
        let tool_ceiling = tool_ceiling.clone();

        Ok(Box::new(move |principal| {
            self.build_with_resolved_backend(
                principal,
                resolved_backend_id,
                provider_kind,
                endpoint,
                api_key,
                api_key_env_var,
                &tool_ceiling,
            )
            .map_err(|error| BehaviorBuildError {
                behavior_id: behavior_name,
                error,
            })
        }))
    }

    #[allow(dead_code)]
    async fn build(
        self,
        node: &EmbeddedNode,
        principal: Arc<AgentPrincipal>,
        tool_ceiling: &ToolCeiling,
    ) -> Result<AgentBehavior> {
        let backend_id = self
            .backend_id
            .as_deref()
            .ok_or_else(|| anyhow!("behavior '{}' is missing backend_id", self.name))?;
        let backend = lookup_backend(node, backend_id).await?.ok_or_else(|| {
            anyhow!(
                "behavior '{}' references missing backend {}",
                self.name,
                backend_id
            )
        })?;
        if !backend.is_available() {
            anyhow::bail!(
                "behavior '{}' backend {} is unavailable (enabled={} probe_status={})",
                self.name,
                backend_id,
                backend.enabled,
                backend.probe_status
            );
        }
        BackendAdmissionConfig::from_backend(&backend)?;
        self.build_with_resolved_backend(
            principal,
            Some(backend.backend_id),
            backend.provider_kind,
            backend.endpoint,
            backend.api_key,
            backend.api_key_env_var,
            tool_ceiling,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_with_resolved_backend(
        self,
        principal: Arc<AgentPrincipal>,
        backend_id: Option<String>,
        backend_provider_kind: BackendProviderKind,
        backend_endpoint: String,
        backend_api_key: Option<String>,
        backend_api_key_env_var: Option<String>,
        tool_ceiling: &ToolCeiling,
    ) -> Result<AgentBehavior> {
        let behavior_name = self.name.clone();

        Ok(AgentBehavior {
            behavior_id: self.name,
            principal,
            backend_id,
            backend_provider_kind,
            backend_endpoint,
            backend_api_key,
            backend_api_key_env_var,
            model_name: self.model_name,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            max_turns: self.max_turns,
            system_prompt: self.system_prompt,
            tools: BehaviorToolConfig::from_selection(
                &behavior_name,
                self.tool_selection,
                tool_ceiling,
                self.custom_tools,
            )?,
            compaction_threshold: self.compaction_threshold,
            compaction_strategy: self.compaction_strategy,
            stream_batch_ms: self.stream_batch_ms,
            deadline_duration: self.deadline_duration,
            sampling: self.sampling,
        })
    }
}

#[cfg(test)]
impl PendingAgentBehavior {
    pub(crate) fn build_with_identity_for_test<I>(self, identity: I) -> AgentBehavior
    where
        I: AgentIdentity + 'static,
    {
        let backend_id = self.backend_id.clone();
        let backend_endpoint = self.backend_endpoint.clone();
        let behavior_name = self.name.clone();
        let identity: Arc<dyn AgentIdentity> = Arc::new(identity);
        let principal = Arc::new(AgentPrincipal {
            agent_did: identity.did().to_string(),
            identity,
            default_behavior_id: behavior_name.clone(),
            display_name: None,
            enabled: true,
        });
        self.build_with_resolved_backend(
            principal,
            backend_id,
            BackendProviderKind::OpenAiCompatible,
            backend_endpoint,
            None,
            None,
            &ToolCeiling::meta_only(),
        )
        .unwrap()
    }
}
