use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;
use tokio::sync::{watch, OnceCell};

use crate::compaction::CompactionStrategy;
use crate::config::{
    AgentBehavior, SamplingConfig, DEFAULT_COMPACTION_THRESHOLD, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS,
    DEFAULT_MODEL_NAME, DEFAULT_STREAM_BATCH_MS,
};
use crate::health_checker::HealthCheckerOptions;
use crate::hook::{BackgroundExecutionRegistry, FailurePolicy};
use crate::identity::{AgentIdentity, AgentPrincipal};
use crate::mcp_pool::McpPool;
use crate::migration;
use crate::retry::RetryPolicy;
use crate::runtime_snapshot::ResolvedRuntimeSnapshot;
use crate::tool_surface::{
    BashMode, BehaviorToolConfig, FileToolMode, SubagentToolConfig, ToolCeiling, ToolSelection,
};
use crate::toolset::{
    default_read_only_command_policy, parse_argv_prefixes, CommandExecutionMode,
    CommandExecutionPolicy, CommandNetworkMode,
};
use crate::trigger_engine::manual_source::ManualTriggerHandle;

mod builder;
mod daemon;
mod document_view;
pub(crate) mod principal_assembly;
mod reconcile;
mod runtime;
mod stream_processor;
#[cfg(test)]
mod supervision;
#[cfg(test)]
mod tests;

pub(crate) use principal_assembly::assemble_principal_and_behaviors;
pub(crate) use principal_assembly::BehaviorBuildError;

#[cfg(test)]
pub(crate) use builder::PendingAgentBehavior;
pub use builder::{BehaviorBuilder, DefraAgentBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLifecycleState {
    Uninitialized,
    Recovering,
    Ready,
    ShuttingDown,
    Shutdown,
}

impl ProcessLifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uninitialized => "uninitialized",
            Self::Recovering => "recovering",
            Self::Ready => "ready",
            Self::ShuttingDown => "shuttingDown",
            Self::Shutdown => "shutdown",
        }
    }
}

pub trait ProcessLifecycleObserver: Send + Sync {
    fn on_process_state_change(&self, state: ProcessLifecycleState);
}

#[derive(Default)]
pub struct DocumentRuntimeOptions {
    pub tool_ceiling: ToolCeiling,
    pub mcp_pool: McpPool,
    pub local_hostname: Option<String>,
    pub local_subnet: Option<String>,
    pub retry_policy: RetryPolicy,
    pub hook_failure_policy: FailurePolicy,
    pub health_checker_options: HealthCheckerOptions,
    pub process_state_observer: Option<Arc<dyn ProcessLifecycleObserver>>,
}

#[derive(Clone)]
pub(crate) struct DocumentResolveContext {
    pub(crate) identity: Arc<dyn AgentIdentity>,
    pub(crate) tool_ceiling: ToolCeiling,
}

#[derive(Clone)]
pub struct DefraAgent {
    node: Arc<EmbeddedNode>,
    principal: Arc<AgentPrincipal>,
    behaviors: Vec<Arc<AgentBehavior>>,
    unavailable_behaviors: HashMap<String, String>,
    document_runtime_context: Option<DocumentResolveContext>,
    mcp_pool: McpPool,
    local_hostname: String,
    local_subnet: Option<String>,
    retry_policy: RetryPolicy,
    hook_failure_policy: FailurePolicy,
    background_execution_registry: BackgroundExecutionRegistry,
    health_checker_options: HealthCheckerOptions,
    process_state_observer: Option<Arc<dyn ProcessLifecycleObserver>>,
    /// Populated once the runtime's `TriggerEngine` has constructed the
    /// `ManualSource`. In-process callers that cloned this `DefraAgent`
    /// before calling `run()` can then observe the handle via
    /// [`Self::manual_trigger_handle`].
    pub(crate) manual_trigger_handle: Arc<OnceCell<ManualTriggerHandle>>,
}

impl DefraAgent {
    pub fn builder() -> DefraAgentBuilder {
        DefraAgentBuilder::new()
    }

    pub async fn from_default_behavior_documents(
        node: Arc<EmbeddedNode>,
        identity: Arc<dyn AgentIdentity>,
        options: DocumentRuntimeOptions,
    ) -> anyhow::Result<Self> {
        // Run the AgentBehavior migration before any behavior read so that
        // desktops, embedders, and CLI serve paths all see description/summary
        // even when the DB was created before branch #377. This is idempotent
        // (field-presence-checked) and cheap on already-migrated DBs.
        migration::ensure_agent_behavior_migrations(node.clone()).await?;
        let document_runtime_context = DocumentResolveContext {
            identity: identity.clone(),
            tool_ceiling: options.tool_ceiling.clone(),
        };
        let resolved_snapshot =
            resolve_document_runtime_snapshot(node.as_ref(), &document_runtime_context).await?;
        debug_assert!(
            resolved_snapshot.principal.is_some(),
            "from_default_behavior_documents called with a snapshot lacking a principal; \
             the production loader always sets principal: Some(...) — a None snapshot \
             means a non-production path bypassed the loader and would produce a \
             DefraAgent.principal that's NOT Arc::ptr_eq to the snapshot's behavior principals",
        );
        // The snapshot carries the principal Arc constructed once in the loader.
        // Fall back to a synthetic principal if (in tests) the snapshot has none.
        let principal = resolved_snapshot.principal.clone().unwrap_or_else(|| {
            let default_behavior_id = resolved_snapshot.default_behavior_id.clone();
            Arc::new(AgentPrincipal {
                agent_did: identity.did().to_string(),
                identity: identity.clone(),
                default_behavior_id,
                display_name: None,
                enabled: true,
            })
        });
        let default_behavior_id = principal.default_behavior_id.clone();
        let mut behaviors = resolved_snapshot
            .behaviors
            .values()
            .cloned()
            .collect::<Vec<_>>();
        behaviors.sort_by(|left, right| {
            let left_is_default = left.behavior_id == default_behavior_id;
            let right_is_default = right.behavior_id == default_behavior_id;
            right_is_default
                .cmp(&left_is_default)
                .then_with(|| left.behavior_id.cmp(&right.behavior_id))
        });

        Ok(Self {
            node,
            principal,
            behaviors,
            unavailable_behaviors: resolved_snapshot.unavailable_behaviors,
            document_runtime_context: Some(document_runtime_context),
            mcp_pool: options.mcp_pool,
            local_hostname: options
                .local_hostname
                .unwrap_or_else(runtime::default_hostname),
            local_subnet: options.local_subnet,
            retry_policy: options.retry_policy,
            hook_failure_policy: options.hook_failure_policy,
            background_execution_registry: BackgroundExecutionRegistry::default(),
            health_checker_options: options.health_checker_options,
            process_state_observer: options.process_state_observer,
            manual_trigger_handle: Arc::new(OnceCell::new()),
        })
    }

    pub fn behaviors(&self) -> &[Arc<AgentBehavior>] {
        &self.behaviors
    }

    /// Returns the deployment principal record.
    ///
    /// All DefraDB ops issued by this `DefraAgent` are signed by
    /// `self.principal.identity`. Two `AgentBehavior`s on the same
    /// `DefraAgent` share this Arc by construction (single-principal
    /// per snapshot invariant), so any DID-keyed permission decision
    /// returns identical results for behaviors on this deployment.
    pub fn principal(&self) -> &AgentPrincipal {
        &self.principal
    }

    /// Returns a clone of the deployment principal Arc.
    ///
    /// Use this when threading the principal into a task / spawned
    /// future / snapshot rebuild path that needs to hold the Arc
    /// independently. Prefer `principal()` for read-only access
    /// from a single scope.
    pub(crate) fn principal_arc(&self) -> Arc<AgentPrincipal> {
        Arc::clone(&self.principal)
    }

    pub fn agent_did(&self) -> &str {
        &self.principal.agent_did
    }

    pub fn default_behavior_id(&self) -> &str {
        &self.principal.default_behavior_id
    }

    pub fn unavailable_behaviors(&self) -> &HashMap<String, String> {
        &self.unavailable_behaviors
    }

    pub fn background_execution_registry(&self) -> BackgroundExecutionRegistry {
        self.background_execution_registry.clone()
    }

    pub(crate) fn document_runtime_context(&self) -> Option<&DocumentResolveContext> {
        self.document_runtime_context.as_ref()
    }

    /// Returns the `ManualTriggerHandle` once the runtime's `TriggerEngine`
    /// has been brought up.
    ///
    /// `None` means the runtime is still in early bootstrap (the trigger
    /// engine spawns after `run()` resolves the initial snapshot and passes
    /// the startup barrier). In-process callers that need to push manual
    /// fires should clone this `DefraAgent`, spawn `run()` on one copy, and
    /// poll the clone until this returns `Some`.
    #[allow(dead_code)] // consumed by CLI (Task 8) and desktop (Task 10)
    pub(crate) fn manual_trigger_handle(&self) -> Option<&ManualTriggerHandle> {
        self.manual_trigger_handle.get()
    }

    pub async fn run(self, shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        runtime::run_agent(self, shutdown).await
    }
}

pub(crate) async fn resolve_document_runtime_snapshot(
    node: &EmbeddedNode,
    context: &DocumentResolveContext,
) -> anyhow::Result<ResolvedRuntimeSnapshot> {
    let view = document_view::load_document_runtime_view(node, context.identity.did()).await?;
    document_view::resolve_document_runtime_snapshot_from_view(node, context, &view).await
}

pub(crate) fn behavior_config_from_documents(
    principal: Arc<AgentPrincipal>,
    behavior: &crate::document_config::AgentBehavior,
    backend: &crate::backend_registry::InferenceBackend,
    inference_profile: &crate::document_config::InferenceProfile,
    tool_selection: ToolSelection,
    subagent_tools: SubagentToolConfig,
    tool_ceiling: &ToolCeiling,
    skills: Vec<crate::skills::Skill>,
) -> anyhow::Result<AgentBehavior> {
    let compaction_strategy = parse_compaction_strategy(behavior.compaction_strategy.as_deref())?;
    let stream_batch_ms = inference_profile
        .stream_batch_ms
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(DEFAULT_STREAM_BATCH_MS);
    let deadline_duration_secs = inference_profile
        .deadline_duration_secs
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(DEFAULT_DEADLINE_DURATION_SECS);
    let profile_max_tokens = inference_profile
        .max_output_tokens
        .and_then(|value| u64::try_from(value).ok());

    Ok(AgentBehavior {
        behavior_id: behavior.behavior_id.clone(),
        principal,
        backend_id: Some(backend.backend_id.clone()),
        backend_provider_kind: backend.provider_kind,
        backend_endpoint: backend.endpoint.clone(),
        backend_api_key: backend.api_key.clone(),
        backend_api_key_env_var: backend.api_key_env_var.clone(),
        model_name: normalize_optional_string(behavior.model_name.as_deref())
            .unwrap_or(DEFAULT_MODEL_NAME)
            .to_string(),
        context_window: inference_profile
            .context_window
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(DEFAULT_CONTEXT_WINDOW),
        max_output_tokens: inference_profile
            .max_output_tokens
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
        max_turns: inference_profile
            .max_turns
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(DEFAULT_MAX_TURNS),
        system_prompt: behavior.system_prompt.clone().unwrap_or_default(),
        tools: BehaviorToolConfig::from_selection_with_subagent_tools(
            &behavior.behavior_id,
            tool_selection,
            tool_ceiling,
            subagent_tools,
            Vec::new(),
        )?,
        compaction_threshold: behavior
            .compaction_threshold
            .unwrap_or(DEFAULT_COMPACTION_THRESHOLD),
        compaction_strategy,
        stream_batch_ms,
        deadline_duration: Duration::from_secs(deadline_duration_secs),
        sampling: SamplingConfig {
            temperature: inference_profile.temperature,
            top_p: None,
            top_k: None,
            max_tokens: profile_max_tokens,
        },
        skills,
    })
}

fn parse_compaction_strategy(value: Option<&str>) -> anyhow::Result<CompactionStrategy> {
    match normalize_optional_string(value) {
        None => Ok(CompactionStrategy::StripThenSummarize),
        Some("StripToolResults") => Ok(CompactionStrategy::StripToolResults),
        Some("Summarize") => Ok(CompactionStrategy::Summarize),
        Some("StripThenSummarize") => Ok(CompactionStrategy::StripThenSummarize),
        Some(other) => anyhow::bail!("unknown compaction strategy {other}"),
    }
}

fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(crate) fn tool_selection_from_document(
    selection: &crate::document_config::ToolSelectionDocument,
) -> anyhow::Result<ToolSelection> {
    let bash = if selection.enable_bash.unwrap_or(false) {
        BashMode::parse(selection.bash_mode.as_deref().unwrap_or("ReadOnly"))?
    } else {
        BashMode::Off
    };
    Ok(ToolSelection {
        file_tools: if selection.enable_file_tools.unwrap_or(false) {
            FileToolMode::parse(selection.file_tools_mode.as_deref().unwrap_or("ReadOnly"))?
        } else {
            FileToolMode::Off
        },
        file_tool_root: normalize_optional_string(selection.file_tool_root.as_deref())
            .map(std::path::PathBuf::from),
        bash,
        command_policy: command_policy_from_document(selection, bash)?,
        cli_tool_names: selection.cli_tool_names.clone().unwrap_or_default(),
        enable_meta_tools: selection.enable_meta_tools.unwrap_or(true),
        allowed_mcp_service_ids: selection
            .allowed_mcp_service_ids
            .clone()
            .unwrap_or_default(),
        backgroundable_tool_names: selection
            .backgroundable_tool_names
            .clone()
            .unwrap_or_default(),
        // This opt-in is intentionally default-off. The memory tool is also
        // behind the non-default `agent-memory` Cargo feature.
        enable_memory: selection.enable_memory.unwrap_or(false),
        // The `defra_query` read tool defaults on with all collections; an
        // operator can disable it or restrict its collection scope per behavior
        // via the ToolSelection document. (A built-in guard always blocks
        // sensitive fields regardless of this scope.)
        enable_defra_query: selection.enable_defra_query.unwrap_or(true),
        defra_query_collections: selection
            .defra_query_collections
            .clone()
            .unwrap_or_default(),
    })
}

pub(crate) fn subagent_tool_config_from_document(
    selection: &crate::document_config::ToolSelectionDocument,
) -> SubagentToolConfig {
    let targets = selection
        .subagent_targets
        .iter()
        .flatten()
        .filter_map(
            |entry| match crate::document_config::SubagentTarget::parse(entry) {
                Ok(target) => Some(target),
                Err(error) => {
                    tracing::warn!(
                        selection_id = %selection.selection_id,
                        entry = %entry,
                        %error,
                        "skipping malformed subagent_targets entry"
                    );
                    None
                }
            },
        )
        .collect();
    SubagentToolConfig {
        targets,
        spawn_enabled: selection.subagent_spawn_enabled.unwrap_or(false),
        steering_enabled: selection.subagent_steering_enabled.unwrap_or(false),
        background_enabled: selection.subagent_background_enabled.unwrap_or(false),
        allow_cross_deployment: selection.subagent_allow_cross_deployment.unwrap_or(false),
    }
}

fn command_policy_from_document(
    selection: &crate::document_config::ToolSelectionDocument,
    bash: BashMode,
) -> anyhow::Result<Option<CommandExecutionPolicy>> {
    let has_policy = selection
        .command_execution_policy
        .as_deref()
        .and_then(|value| normalize_optional_string(Some(value)))
        .is_some()
        || selection
            .command_network_mode
            .as_deref()
            .and_then(|value| normalize_optional_string(Some(value)))
            .is_some()
        || selection
            .command_allowed_argv_prefixes
            .as_ref()
            .is_some_and(|prefixes| !prefixes.is_empty())
        || selection
            .command_forbidden_argv_prefixes
            .as_ref()
            .is_some_and(|prefixes| !prefixes.is_empty());
    if !has_policy {
        return if matches!(bash, BashMode::Unrestricted) {
            Ok(Some(
                CommandExecutionPolicy::write_capable()
                    .with_mode(CommandExecutionMode::Unrestricted),
            ))
        } else {
            Ok(None)
        };
    }

    let requested_mode = selection
        .command_execution_policy
        .as_deref()
        .and_then(|value| normalize_optional_string(Some(value)))
        .map(CommandExecutionMode::parse)
        .transpose()?;
    let mode = match bash {
        BashMode::Off => CommandExecutionMode::ReadOnly,
        BashMode::ReadOnly => CommandExecutionMode::ReadOnly,
        BashMode::Unrestricted => requested_mode.unwrap_or(CommandExecutionMode::Unrestricted),
    };

    let allowed = parse_argv_prefixes(
        selection
            .command_allowed_argv_prefixes
            .as_deref()
            .unwrap_or(&[]),
    )?;
    let forbidden = parse_argv_prefixes(
        selection
            .command_forbidden_argv_prefixes
            .as_deref()
            .unwrap_or(&[]),
    )?;
    let network_mode = selection
        .command_network_mode
        .as_deref()
        .and_then(|value| normalize_optional_string(Some(value)))
        .map(CommandNetworkMode::parse)
        .transpose()?
        .unwrap_or(CommandNetworkMode::Inherit);

    let base = if matches!(mode, CommandExecutionMode::ReadOnly) {
        default_read_only_command_policy()
    } else {
        CommandExecutionPolicy::write_capable()
    };
    Ok(Some(
        base.with_mode(mode)
            .with_allowed_argv_prefixes(allowed)
            .with_forbidden_argv_prefixes(forbidden)
            .with_network_mode(network_mode),
    ))
}
