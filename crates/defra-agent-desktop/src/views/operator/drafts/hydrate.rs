use crate::client::ClientStore;
use crate::state::{
    BackendDraft, BehaviorDraft, InferenceProfileDraft, OperatorDraft, OperatorSection,
    ScheduledTaskDraft, ToolSelectionDraft,
};

use super::{backend_ids_for_agent, inference_profile_ids_for_agent};

pub(super) fn draft_for_selection(
    store: &ClientStore,
    section: OperatorSection,
    selected_agent_did: Option<&str>,
    entity_id: &str,
) -> Option<OperatorDraft> {
    match section {
        OperatorSection::Behaviors => store
            .behaviors
            .iter()
            .find(|row| {
                row.behavior_id == entity_id && row.agent_did.as_deref() == selected_agent_did
            })
            .map(|row| {
                OperatorDraft::Behavior(BehaviorDraft {
                    behavior_id: row.behavior_id.clone(),
                    agent_did: row.agent_did.clone().unwrap_or_default(),
                    display_name: row.display_name.clone().unwrap_or_default(),
                    system_prompt: row.system_prompt.clone().unwrap_or_default(),
                    backend_id: row.backend_id.clone().unwrap_or_default(),
                    model_name: row.model_name.clone().unwrap_or_default(),
                    tool_selection_id: row.tool_selection_id.clone().unwrap_or_default(),
                    inference_profile_id: row.inference_profile_id.clone().unwrap_or_default(),
                    compaction_strategy: row.compaction_strategy.clone().unwrap_or_default(),
                    compaction_threshold: row
                        .compaction_threshold
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    enabled: row.enabled.unwrap_or(true),
                    created_at: row.created_at.clone().unwrap_or_default(),
                })
            }),
        OperatorSection::Backends => {
            let backend_ids = backend_ids_for_agent(store, selected_agent_did);
            store
                .inference_backends
                .iter()
                .find(|row| {
                    row.backend_id == entity_id && backend_ids.contains(&row.backend_id.as_str())
                })
                .map(|row| {
                    OperatorDraft::Backend(BackendDraft {
                        backend_id: row.backend_id.clone(),
                        name: row.name.clone().unwrap_or_default(),
                        provider_kind: row.provider_kind.clone().unwrap_or_default(),
                        endpoint: row.endpoint.clone().unwrap_or_default(),
                        api_key: row.api_key.clone().unwrap_or_default(),
                        api_key_env_var: row.api_key_env_var.clone().unwrap_or_default(),
                        max_concurrent: row
                            .max_concurrent
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                        max_queue_depth: row
                            .max_queue_depth
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                        enabled: row.enabled.unwrap_or(true),
                        models: row.models.join(", "),
                        probe_status: row.probe_status.clone().unwrap_or_default(),
                    })
                })
        }
        OperatorSection::ToolSelections => store
            .tool_selections
            .iter()
            .find(|row| {
                row.selection_id == entity_id && row.agent_did.as_deref() == selected_agent_did
            })
            .map(|row| {
                OperatorDraft::ToolSelection(ToolSelectionDraft {
                    selection_id: row.selection_id.clone(),
                    agent_did: row.agent_did.clone().unwrap_or_default(),
                    display_name: row.display_name.clone().unwrap_or_default(),
                    enable_file_tools: row.enable_file_tools.unwrap_or(false),
                    file_tools_mode: row.file_tools_mode.clone().unwrap_or_default(),
                    enable_bash: row.enable_bash.unwrap_or(false),
                    bash_mode: row.bash_mode.clone().unwrap_or_default(),
                    cli_tool_names: row.cli_tool_names.join(", "),
                    enable_meta_tools: row.enable_meta_tools.unwrap_or(false),
                    delegate_to: row.delegate_to.join(", "),
                })
            }),
        OperatorSection::InferenceProfiles => {
            let profile_ids = inference_profile_ids_for_agent(store, selected_agent_did);
            store
                .inference_profiles
                .iter()
                .find(|row| {
                    row.profile_id == entity_id && profile_ids.contains(&row.profile_id.as_str())
                })
                .map(|row| {
                    OperatorDraft::InferenceProfile(InferenceProfileDraft {
                        profile_id: row.profile_id.clone(),
                        display_name: row.display_name.clone().unwrap_or_default(),
                        context_window: row
                            .context_window
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                        max_output_tokens: row
                            .max_output_tokens
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                        max_turns: row
                            .max_turns
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                        temperature: row
                            .temperature
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                        stream_batch_ms: row
                            .stream_batch_ms
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                        deadline_duration_secs: row
                            .deadline_duration_secs
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                    })
                })
        }
        OperatorSection::ScheduledTasks => store
            .scheduled_tasks
            .iter()
            .find(|row| row.task_id == entity_id && row.agent_did.as_deref() == selected_agent_did)
            .map(|row| {
                OperatorDraft::ScheduledTask(ScheduledTaskDraft {
                    task_id: row.task_id.clone(),
                    agent_did: row.agent_did.clone().unwrap_or_default(),
                    behavior_id: row.behavior_id.clone().unwrap_or_default(),
                    name: row.name.clone().unwrap_or_default(),
                    prompt: row.prompt.clone().unwrap_or_default(),
                    interval_secs: row
                        .interval_secs
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    enabled: row.enabled.unwrap_or(true),
                    next_run_at: row.next_run_at.clone().unwrap_or_default(),
                    last_run_at: row.last_run_at.clone().unwrap_or_default(),
                    last_status: row.last_status.clone().unwrap_or_default(),
                    last_error: row.last_error.clone().unwrap_or_default(),
                    run_count: row
                        .run_count
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    created_at: row.created_at.clone().unwrap_or_default(),
                    updated_at: row.updated_at.clone().unwrap_or_default(),
                })
            }),
        _ => None,
    }
}
