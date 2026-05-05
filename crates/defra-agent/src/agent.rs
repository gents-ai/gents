use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;
use tokio::sync::{watch, OnceCell};

use crate::compaction::CompactionStrategy;
use crate::config::{
    BehaviorConfig, SamplingConfig, DEFAULT_COMPACTION_THRESHOLD, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS,
    DEFAULT_MODEL_NAME, DEFAULT_STREAM_BATCH_MS,
};
use crate::hook::FailurePolicy;
use crate::identity::AgentIdentity;
use crate::mcp_pool::McpPool;
use crate::retry::RetryPolicy;
use crate::runtime_snapshot::ResolvedRuntimeSnapshot;
use crate::tool_surface::{BashMode, BehaviorToolConfig, FileToolMode, ToolCeiling, ToolSelection};
use crate::toolset::{
    default_read_only_command_policy, parse_argv_prefixes, CommandExecutionMode,
    CommandExecutionPolicy, CommandNetworkMode,
};
use crate::trigger_engine::manual_source::ManualTriggerHandle;

mod builder;
mod daemon;
mod document_view;
mod reconcile;
mod runtime;
mod stream_processor;
#[cfg(test)]
mod supervision;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use builder::PendingBehaviorConfig;
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
    agent_did: String,
    default_behavior_id: String,
    behaviors: Vec<Arc<BehaviorConfig>>,
    unavailable_behaviors: HashMap<String, String>,
    document_runtime_context: Option<DocumentResolveContext>,
    mcp_pool: McpPool,
    local_hostname: String,
    local_subnet: Option<String>,
    retry_policy: RetryPolicy,
    hook_failure_policy: FailurePolicy,
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
        let document_runtime_context = DocumentResolveContext {
            identity: identity.clone(),
            tool_ceiling: options.tool_ceiling.clone(),
        };
        let resolved_snapshot =
            resolve_document_runtime_snapshot(node.as_ref(), &document_runtime_context).await?;
        let default_behavior_id = resolved_snapshot.default_behavior_id.clone();
        let mut behaviors = resolved_snapshot
            .behaviors
            .values()
            .cloned()
            .collect::<Vec<_>>();
        behaviors.sort_by(|left, right| {
            let left_is_default = left.name == default_behavior_id;
            let right_is_default = right.name == default_behavior_id;
            right_is_default
                .cmp(&left_is_default)
                .then_with(|| left.name.cmp(&right.name))
        });

        Ok(Self {
            node,
            agent_did: identity.did().to_string(),
            default_behavior_id,
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
            process_state_observer: options.process_state_observer,
            manual_trigger_handle: Arc::new(OnceCell::new()),
        })
    }

    pub fn behaviors(&self) -> &[Arc<BehaviorConfig>] {
        &self.behaviors
    }

    pub fn agent_did(&self) -> &str {
        &self.agent_did
    }

    pub fn default_behavior_id(&self) -> &str {
        &self.default_behavior_id
    }

    pub fn unavailable_behaviors(&self) -> &HashMap<String, String> {
        &self.unavailable_behaviors
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
    identity: Arc<dyn AgentIdentity>,
    behavior: &crate::document_config::AgentBehavior,
    backend: &crate::backend_registry::InferenceBackend,
    inference_profile: &crate::document_config::InferenceProfile,
    tool_selection: ToolSelection,
    tool_ceiling: &ToolCeiling,
) -> anyhow::Result<BehaviorConfig> {
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

    Ok(BehaviorConfig {
        name: behavior.behavior_id.clone(),
        identity,
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
        tools: BehaviorToolConfig::from_selection(
            &behavior.behavior_id,
            tool_selection,
            tool_ceiling,
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
        delegate_to: selection.delegate_to.clone().unwrap_or_default(),
    })
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
        return Ok(None);
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
        BashMode::Unrestricted => requested_mode.unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                CommandExecutionMode::WorkspaceWrite
            } else {
                CommandExecutionMode::Unrestricted
            }
        }),
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
