use anyhow::{anyhow, bail, Result};
use defra_agent_desktop_core::client::ClientCore;
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentPrincipalRow, InferenceBackendRow, InferenceProfileRow, ToolSelectionRow,
};

use super::super::types::{
    AgentConfigSaveRequest, BackendSaveRequest, BehaviorSaveRequest, InferenceProfileSaveRequest,
    ToolSelectionSaveRequest,
};
use super::util::{require_trimmed, trim_optional};

pub(crate) async fn save_agent_config(
    core: &ClientCore,
    request: AgentConfigSaveRequest,
) -> Result<()> {
    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let display_name = require_trimmed("display_name", request.display_name)?;
    let default_behavior_id = require_trimmed("default_behavior_id", request.default_behavior_id)?;

    let store = core.store().snapshot();
    if !store.behaviors.iter().any(|behavior| {
        behavior.agent_did.as_deref() == Some(agent_did.as_str())
            && behavior.behavior_id == default_behavior_id
    }) {
        bail!("default_behavior_id {default_behavior_id} does not exist for {agent_did}");
    }

    let mut row = store
        .agent_principals
        .iter()
        .find(|row| row.agent_did == agent_did)
        .cloned()
        .unwrap_or_else(|| AgentPrincipalRow {
            agent_did: agent_did.clone(),
            display_name: None,
            default_behavior_id: None,
            enabled: Some(true),
            created_at: None,
            created_by: Some(agent_did.clone()),
        });
    row.display_name = Some(display_name);
    row.default_behavior_id = Some(default_behavior_id);
    row.enabled = Some(request.enabled.unwrap_or(true));
    core.save_agent_principal(&row).await?;
    Ok(())
}

pub(crate) async fn save_behavior_config(
    core: &ClientCore,
    request: BehaviorSaveRequest,
) -> Result<()> {
    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let behavior_id = require_trimmed("behavior_id", request.behavior_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    let store = core.store().snapshot();
    let mut row = store
        .behavior_row(&agent_did, &behavior_id)
        .cloned()
        .unwrap_or_else(|| AgentBehaviorRow {
            behavior_id: behavior_id.clone(),
            agent_did: Some(agent_did.clone()),
            display_name: None,
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: Some(true),
            created_at: None,
        });
    let inference_profile_id = trim_optional(request.inference_profile_id)
        .ok_or_else(|| anyhow!("inference_profile_id is required"))?;
    if !store
        .inference_profiles
        .iter()
        .any(|profile| profile.profile_id == inference_profile_id)
    {
        bail!("inference_profile_id {inference_profile_id} does not exist");
    }
    row.display_name = Some(display_name);
    row.agent_did = Some(agent_did);
    row.system_prompt = Some(request.system_prompt);
    row.backend_id = trim_optional(request.backend_id);
    row.tool_selection_id = trim_optional(request.tool_selection_id);
    row.inference_profile_id = Some(inference_profile_id);
    row.compaction_strategy = trim_optional(request.compaction_strategy);
    row.compaction_threshold = request.compaction_threshold;
    row.enabled = request.enabled.or(row.enabled).or(Some(true));
    if let Some(backend_id) = row.backend_id.as_deref() {
        if let Some(model_name) = store
            .inference_backends
            .iter()
            .find(|backend| backend.backend_id == backend_id)
            .and_then(|backend| backend.models.first())
            .cloned()
        {
            row.model_name = Some(model_name);
        }
    }
    core.save_behavior(&row).await?;
    Ok(())
}

pub(crate) async fn save_backend_config(
    core: &ClientCore,
    request: BackendSaveRequest,
) -> Result<()> {
    let backend_id = require_trimmed("backend_id", request.backend_id)?;
    let name = require_trimmed("name", request.name)?;
    let provider_kind = require_trimmed("provider_kind", request.provider_kind)?;
    let endpoint = require_trimmed("endpoint", request.endpoint)?;
    let models = request
        .models
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if models.is_empty() {
        bail!("at least one model is required");
    }

    let store = core.store().snapshot();
    let mut row = store
        .inference_backends
        .iter()
        .find(|row| row.backend_id == backend_id)
        .cloned()
        .unwrap_or_else(|| InferenceBackendRow {
            backend_id: backend_id.clone(),
            name: None,
            provider_kind: None,
            endpoint: None,
            api_key: None,
            api_key_env_var: None,
            max_concurrent: None,
            max_queue_depth: None,
            enabled: Some(true),
            models: Vec::new(),
            last_probe: None,
            probe_status: None,
        });
    row.name = Some(name);
    row.provider_kind = Some(provider_kind);
    row.endpoint = Some(endpoint);
    if request.clear_api_key.unwrap_or(false) {
        row.api_key = None;
    } else if request.api_key.is_some() {
        row.api_key = trim_optional(request.api_key);
    }
    if request.api_key_env_var.is_some() {
        row.api_key_env_var = trim_optional(request.api_key_env_var);
    }
    row.models = models;
    row.max_concurrent = request.max_concurrent;
    row.max_queue_depth = request.max_queue_depth;
    row.enabled = request.enabled.or(row.enabled).or(Some(true));
    row.probe_status = Some("healthy".to_string());
    core.save_backend(&row).await?;
    Ok(())
}

pub(crate) async fn save_inference_profile_config(
    core: &ClientCore,
    request: InferenceProfileSaveRequest,
) -> Result<()> {
    let profile_id = require_trimmed("profile_id", request.profile_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    let store = core.store().snapshot();
    let mut row = store
        .inference_profiles
        .iter()
        .find(|row| row.profile_id == profile_id)
        .cloned()
        .unwrap_or_else(|| InferenceProfileRow {
            profile_id: profile_id.clone(),
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            max_turns: None,
            temperature: None,
            stream_batch_ms: None,
            deadline_duration_secs: None,
        });
    row.display_name = Some(display_name);
    row.context_window = request.context_window;
    row.max_output_tokens = request.max_output_tokens;
    row.max_turns = request.max_turns;
    row.temperature = request.temperature;
    row.stream_batch_ms = request.stream_batch_ms;
    row.deadline_duration_secs = request.deadline_duration_secs;
    core.save_inference_profile(&row).await?;
    Ok(())
}

pub(crate) async fn save_tool_selection_config(
    core: &ClientCore,
    request: ToolSelectionSaveRequest,
) -> Result<()> {
    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let selection_id = require_trimmed("selection_id", request.selection_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    let store = core.store().snapshot();
    let mut row = store
        .tool_selections
        .iter()
        .find(|row| row.selection_id == selection_id)
        .cloned()
        .unwrap_or_else(|| ToolSelectionRow {
            selection_id: selection_id.clone(),
            agent_did: Some(agent_did.clone()),
            display_name: None,
            enable_file_tools: Some(false),
            file_tools_mode: None,
            file_tool_root: None,
            enable_bash: Some(false),
            bash_mode: None,
            command_execution_policy: None,
            command_allowed_argv_prefixes: Vec::new(),
            command_forbidden_argv_prefixes: Vec::new(),
            command_network_mode: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: Some(false),
            allowed_mcp_service_ids: Vec::new(),
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_defra_query: Some(true),
            defra_query_collections: Vec::new(),
            subagent_targets: Vec::new(),
            subagent_spawn_enabled: Some(false),
            subagent_steering_enabled: Some(false),
            subagent_background_enabled: Some(false),
            cross_deployment_spawn_timeout_seconds: None,
            enable_memory: Some(false),
        });
    row.agent_did = Some(agent_did);
    row.display_name = Some(display_name);
    row.enable_file_tools = request.enable_file_tools.or(row.enable_file_tools);
    row.file_tools_mode = trim_optional(request.file_tools_mode);
    row.file_tool_root = trim_optional(request.file_tool_root);
    row.enable_bash = request.enable_bash.or(row.enable_bash);
    row.bash_mode = trim_optional(request.bash_mode);
    row.command_execution_policy = trim_optional(request.command_execution_policy);
    row.command_allowed_argv_prefixes = request
        .command_allowed_argv_prefixes
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    row.command_forbidden_argv_prefixes = request
        .command_forbidden_argv_prefixes
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    row.command_network_mode = trim_optional(request.command_network_mode);
    row.cli_tool_names = request
        .cli_tool_names
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    row.enable_meta_tools = request.enable_meta_tools.or(row.enable_meta_tools);
    row.allowed_mcp_service_ids = request
        .allowed_mcp_service_ids
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    row.delegate_to = request
        .delegate_to
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    row.backgroundable_tool_names = request
        .backgroundable_tool_names
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    row.subagent_targets = request
        .subagent_targets
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    row.subagent_spawn_enabled = request
        .subagent_spawn_enabled
        .or(row.subagent_spawn_enabled);
    row.subagent_steering_enabled = request
        .subagent_steering_enabled
        .or(row.subagent_steering_enabled);
    row.subagent_background_enabled = request
        .subagent_background_enabled
        .or(row.subagent_background_enabled);
    row.cross_deployment_spawn_timeout_seconds = request.cross_deployment_spawn_timeout_seconds;
    row.enable_memory = request.enable_memory.or(row.enable_memory);
    core.save_tool_selection(&row).await?;
    Ok(())
}
