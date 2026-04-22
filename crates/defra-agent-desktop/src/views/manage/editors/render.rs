use eframe::egui::Ui;

use crate::state::{
    BackendDraft, BehaviorDraft, InferenceProfileDraft, ScheduleDraft, TaskDraft,
    ToolSelectionDraft,
};

use super::{
    editor_heading, multiline_field, read_only_field, read_only_multiline, text_field, toggle_field,
};

pub(super) fn render_behavior_editor(ui: &mut Ui, draft: &mut BehaviorDraft) {
    editor_heading(ui, "Behavior");
    text_field(ui, "Behavior ID", &mut draft.behavior_id);
    text_field(ui, "Agent DID", &mut draft.agent_did);
    text_field(ui, "Display Name", &mut draft.display_name);
    multiline_field(ui, "System Prompt", &mut draft.system_prompt, 8);
    text_field(ui, "Backend ID", &mut draft.backend_id);
    text_field(ui, "Model Name", &mut draft.model_name);
    text_field(ui, "Tool Selection ID", &mut draft.tool_selection_id);
    text_field(ui, "Inference Profile ID", &mut draft.inference_profile_id);
    text_field(ui, "Compaction Strategy", &mut draft.compaction_strategy);
    text_field(ui, "Compaction Threshold", &mut draft.compaction_threshold);
    toggle_field(ui, "Enabled", &mut draft.enabled);
}

pub(super) fn render_backend_editor(ui: &mut Ui, draft: &mut BackendDraft) {
    editor_heading(ui, "Backend");
    text_field(ui, "Backend ID", &mut draft.backend_id);
    text_field(ui, "Name", &mut draft.name);
    text_field(ui, "Provider Kind", &mut draft.provider_kind);
    text_field(ui, "Endpoint", &mut draft.endpoint);
    toggle_field(ui, "Enabled", &mut draft.enabled);
    text_field(ui, "Probe Status", &mut draft.probe_status);
    multiline_field(ui, "Models", &mut draft.models, 4);
    text_field(ui, "API Key", &mut draft.api_key);
    text_field(ui, "API Key Env Var", &mut draft.api_key_env_var);
    text_field(ui, "Max Concurrent", &mut draft.max_concurrent);
    text_field(ui, "Max Queue Depth", &mut draft.max_queue_depth);
}

pub(super) fn render_tool_selection_editor(ui: &mut Ui, draft: &mut ToolSelectionDraft) {
    editor_heading(ui, "Tool Selection");
    text_field(ui, "Selection ID", &mut draft.selection_id);
    text_field(ui, "Agent DID", &mut draft.agent_did);
    text_field(ui, "Display Name", &mut draft.display_name);
    toggle_field(ui, "Enable File Tools", &mut draft.enable_file_tools);
    text_field(ui, "File Tools Mode", &mut draft.file_tools_mode);
    toggle_field(ui, "Enable Bash", &mut draft.enable_bash);
    text_field(ui, "Bash Mode", &mut draft.bash_mode);
    multiline_field(ui, "CLI Tool Names", &mut draft.cli_tool_names, 4);
    toggle_field(ui, "Enable Meta Tools", &mut draft.enable_meta_tools);
    multiline_field(ui, "Delegate To", &mut draft.delegate_to, 4);
}

pub(super) fn render_inference_profile_editor(ui: &mut Ui, draft: &mut InferenceProfileDraft) {
    editor_heading(ui, "Inference Profile");
    text_field(ui, "Profile ID", &mut draft.profile_id);
    text_field(ui, "Display Name", &mut draft.display_name);
    text_field(ui, "Context Window", &mut draft.context_window);
    text_field(ui, "Max Output Tokens", &mut draft.max_output_tokens);
    text_field(ui, "Max Turns", &mut draft.max_turns);
    text_field(ui, "Temperature", &mut draft.temperature);
    text_field(ui, "Stream Batch Ms", &mut draft.stream_batch_ms);
    text_field(
        ui,
        "Deadline Duration Secs",
        &mut draft.deadline_duration_secs,
    );
}

/// Task 51 renders a minimal read-only detail view for tasks. Task 52
/// will replace this with the real editor (description,
/// prompt_template, output_schema_ref wired into mutations).
pub(super) fn render_task_editor(ui: &mut Ui, draft: &mut TaskDraft) {
    editor_heading(ui, "Task");
    text_field(ui, "Task ID", &mut draft.task_id);
    text_field(ui, "Name", &mut draft.name);
    multiline_field(ui, "Description", &mut draft.description, 3);
    text_field(ui, "Behavior ID", &mut draft.behavior_id);
    multiline_field(ui, "Prompt Template", &mut draft.prompt_template, 8);
    toggle_field(ui, "Enabled", &mut draft.enabled);
    text_field(ui, "Output Schema Ref", &mut draft.output_schema_ref);
    read_only_field(ui, "Created At", draft.created_at.as_str());
    read_only_field(ui, "Updated At", draft.updated_at.as_str());
}

/// Task 51 renders a minimal read-only detail view for schedules. Task
/// 52 wires the editor; Task 53 surfaces the fire-bookkeeping fields
/// (last_attempt_at, last_status, last_error, fire_count) cleanly.
pub(super) fn render_schedule_editor(ui: &mut Ui, draft: &mut ScheduleDraft) {
    editor_heading(ui, "Schedule");
    text_field(ui, "Schedule ID", &mut draft.schedule_id);
    text_field(ui, "Task ID", &mut draft.task_id);
    text_field(ui, "Interval Secs", &mut draft.interval_secs);
    toggle_field(ui, "Enabled", &mut draft.enabled);
    text_field(ui, "Concurrency", &mut draft.concurrency);
    text_field(ui, "Next Run At", &mut draft.next_run_at);
    read_only_field(ui, "Last Attempt At", draft.last_attempt_at.as_str());
    read_only_field(ui, "Last Status", draft.last_status.as_str());
    read_only_multiline(ui, "Last Error", draft.last_error.as_str(), 4);
    read_only_field(ui, "Fire Count", draft.fire_count.as_str());
    read_only_field(ui, "Created At", draft.created_at.as_str());
    read_only_field(ui, "Updated At", draft.updated_at.as_str());
}
