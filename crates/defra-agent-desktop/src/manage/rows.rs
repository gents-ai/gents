use anyhow::Result;
use defra_agent_protocol::row::{
    AgentBehaviorRow, InferenceBackendRow, InferenceProfileRow, ScheduleRow, TaskRow,
    ToolSelectionRow,
};

use crate::state::{
    BackendDraft, BehaviorDraft, InferenceProfileDraft, ScheduleDraft, TaskDraft, ToolSelectionDraft,
};

use super::{
    normalize_optional_owned, normalize_required, parse_optional_f64, parse_optional_i64,
    parse_optional_rfc3339, parse_required_positive_i64, split_csv,
};

pub fn behavior_row(draft: &BehaviorDraft) -> Result<AgentBehaviorRow> {
    Ok(AgentBehaviorRow {
        behavior_id: normalize_required("behavior_id", &draft.behavior_id)?.to_string(),
        agent_did: Some(normalize_required("agent_did", &draft.agent_did)?.to_string()),
        display_name: normalize_optional_owned(&draft.display_name),
        system_prompt: normalize_optional_owned(&draft.system_prompt),
        backend_id: normalize_optional_owned(&draft.backend_id),
        model_name: normalize_optional_owned(&draft.model_name),
        tool_selection_id: normalize_optional_owned(&draft.tool_selection_id),
        inference_profile_id: normalize_optional_owned(&draft.inference_profile_id),
        compaction_strategy: normalize_optional_owned(&draft.compaction_strategy),
        compaction_threshold: parse_optional_f64(
            "compaction_threshold",
            &draft.compaction_threshold,
        )?,
        enabled: Some(draft.enabled),
        created_at: normalize_optional_owned(&draft.created_at),
    })
}

pub fn backend_row(draft: &BackendDraft) -> Result<InferenceBackendRow> {
    Ok(InferenceBackendRow {
        backend_id: normalize_required("backend_id", &draft.backend_id)?.to_string(),
        name: normalize_optional_owned(&draft.name),
        provider_kind: normalize_optional_owned(&draft.provider_kind),
        endpoint: normalize_optional_owned(&draft.endpoint),
        api_key: normalize_optional_owned(&draft.api_key),
        api_key_env_var: normalize_optional_owned(&draft.api_key_env_var),
        max_concurrent: parse_optional_i64("max_concurrent", &draft.max_concurrent)?,
        max_queue_depth: parse_optional_i64("max_queue_depth", &draft.max_queue_depth)?,
        enabled: Some(draft.enabled),
        models: split_csv(&draft.models),
        last_probe: None,
        probe_status: normalize_optional_owned(&draft.probe_status),
    })
}

pub fn tool_selection_row(draft: &ToolSelectionDraft) -> Result<ToolSelectionRow> {
    Ok(ToolSelectionRow {
        selection_id: normalize_required("selection_id", &draft.selection_id)?.to_string(),
        agent_did: Some(normalize_required("agent_did", &draft.agent_did)?.to_string()),
        display_name: normalize_optional_owned(&draft.display_name),
        enable_file_tools: Some(draft.enable_file_tools),
        file_tools_mode: normalize_optional_owned(&draft.file_tools_mode),
        enable_bash: Some(draft.enable_bash),
        bash_mode: normalize_optional_owned(&draft.bash_mode),
        cli_tool_names: split_csv(&draft.cli_tool_names),
        enable_meta_tools: Some(draft.enable_meta_tools),
        delegate_to: split_csv(&draft.delegate_to),
    })
}

pub fn inference_profile_row(draft: &InferenceProfileDraft) -> Result<InferenceProfileRow> {
    Ok(InferenceProfileRow {
        profile_id: normalize_required("profile_id", &draft.profile_id)?.to_string(),
        display_name: normalize_optional_owned(&draft.display_name),
        context_window: parse_optional_i64("context_window", &draft.context_window)?,
        max_output_tokens: parse_optional_i64("max_output_tokens", &draft.max_output_tokens)?,
        max_turns: parse_optional_i64("max_turns", &draft.max_turns)?,
        temperature: parse_optional_f64("temperature", &draft.temperature)?,
        stream_batch_ms: parse_optional_i64("stream_batch_ms", &draft.stream_batch_ms)?,
        deadline_duration_secs: parse_optional_i64(
            "deadline_duration_secs",
            &draft.deadline_duration_secs,
        )?,
    })
}

/// Build a `TaskRow` from the draft shown in the Task manage section.
///
/// Task 51 introduces this helper for future editor support (Task 52
/// wires the real editor). The manage-list view itself does not call
/// into this path today.
pub fn task_row(draft: &TaskDraft) -> Result<TaskRow> {
    Ok(TaskRow {
        task_id: normalize_required("task_id", &draft.task_id)?.to_string(),
        name: normalize_optional_owned(&draft.name),
        description: normalize_optional_owned(&draft.description),
        behavior_id: normalize_optional_owned(&draft.behavior_id),
        prompt_template: normalize_optional_owned(&draft.prompt_template),
        enabled: Some(draft.enabled),
        output_schema_ref: normalize_optional_owned(&draft.output_schema_ref),
        created_at: parse_optional_rfc3339("created_at", &draft.created_at)?,
        updated_at: parse_optional_rfc3339("updated_at", &draft.updated_at)?,
    })
}

/// Build a `ScheduleRow` from the draft shown in the Schedule manage
/// section. See `task_row` for scope limits.
pub fn schedule_row(draft: &ScheduleDraft) -> Result<ScheduleRow> {
    Ok(ScheduleRow {
        schedule_id: normalize_required("schedule_id", &draft.schedule_id)?.to_string(),
        task_id: Some(normalize_required("task_id", &draft.task_id)?.to_string()),
        interval_secs: Some(parse_required_positive_i64(
            "interval_secs",
            &draft.interval_secs,
        )?),
        enabled: Some(draft.enabled),
        concurrency: normalize_optional_owned(&draft.concurrency),
        next_run_at: parse_optional_rfc3339("next_run_at", &draft.next_run_at)?,
        last_attempt_at: parse_optional_rfc3339("last_attempt_at", &draft.last_attempt_at)?,
        last_status: normalize_optional_owned(&draft.last_status),
        last_error: normalize_optional_owned(&draft.last_error),
        fire_count: parse_optional_i64("fire_count", &draft.fire_count)?,
        created_at: parse_optional_rfc3339("created_at", &draft.created_at)?,
        updated_at: parse_optional_rfc3339("updated_at", &draft.updated_at)?,
    })
}
