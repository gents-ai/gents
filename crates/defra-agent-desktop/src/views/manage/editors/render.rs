use eframe::egui::Ui;

use crate::client::ClientStore;
use crate::state::{
    BackendDraft, BehaviorDraft, EventTriggerDraft, InferenceProfileDraft, ScheduleDraft, TaskDraft,
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
///
/// The "Recent Runs" section below the apply-owned fields surfaces
/// trigger-engine bookkeeping aggregated across every Schedule and
/// EventTrigger that references this task -- operators can see a
/// single rolled-up fire summary without clicking into every trigger.
/// The bookkeeping values are owned by the trigger engine; the apply
/// writer never projects them, so displaying them here does not
/// threaten the apply/runtime split.
pub(super) fn render_task_editor(ui: &mut Ui, draft: &mut TaskDraft, store: &ClientStore) {
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

    ui.add_space(8.0);
    editor_heading(ui, "Recent Runs");
    let runs = store.recent_runs_for_task(draft.task_id.as_str());
    read_only_field(ui, "Total Fires", &runs.total_fires.to_string());
    read_only_field(
        ui,
        "Last Attempt",
        runs.last_attempt_at.as_deref().unwrap_or("(never)"),
    );
    read_only_field(
        ui,
        "Last Status",
        runs.last_status.as_deref().unwrap_or("(none)"),
    );
    read_only_multiline(ui, "Last Error", runs.last_error.as_deref().unwrap_or(""), 3);
    read_only_field(
        ui,
        "Triggers Referenced",
        &format!(
            "{} schedules, {} event triggers",
            runs.schedule_count, runs.event_trigger_count
        ),
    );
}

/// Schedule detail editor.
///
/// The Schedule collection straddles the apply/runtime boundary: the
/// apply path owns the description of the schedule (`schedule_id`,
/// `task_id`, `interval_secs`, `enabled`, `concurrency`,
/// `created_at`, `updated_at`) while the trigger engine owns the fire
/// bookkeeping (`next_run_at`, `last_attempt_at`, `last_status`,
/// `last_error`, `fire_count`).
///
/// Task 53 groups those halves visually so operators can tell at a
/// glance which fields they can change and which reflect what the
/// scheduler has actually done. The apply-path mutation writer in
/// `client/mutations/manage/task.rs` projects only the apply-owned
/// fields, so the runtime bookkeeping shown here is never clobbered by
/// a desktop save.
pub(super) fn render_schedule_editor(ui: &mut Ui, draft: &mut ScheduleDraft) {
    editor_heading(ui, "Schedule");
    text_field(ui, "Schedule ID", &mut draft.schedule_id);
    text_field(ui, "Task ID", &mut draft.task_id);
    text_field(ui, "Interval Secs", &mut draft.interval_secs);
    toggle_field(ui, "Enabled", &mut draft.enabled);
    text_field(ui, "Concurrency", &mut draft.concurrency);
    read_only_field(ui, "Created At", draft.created_at.as_str());
    read_only_field(ui, "Updated At", draft.updated_at.as_str());

    ui.add_space(8.0);
    editor_heading(ui, "Runtime State");
    // These five fields are owned by the trigger engine. They are
    // shown read-only so the desktop cannot accidentally reset the
    // scheduler's bookkeeping by re-applying a Schedule edit. The
    // mutation writer enforces the same contract at the GraphQL layer
    // by omitting these fields from upsert input.
    read_only_field(ui, "Next Run At", draft.next_run_at.as_str());
    read_only_field(ui, "Last Attempt At", draft.last_attempt_at.as_str());
    read_only_field(ui, "Last Status", draft.last_status.as_str());
    read_only_multiline(ui, "Last Error", draft.last_error.as_str(), 4);
    read_only_field(ui, "Fire Count", draft.fire_count.as_str());
}

/// EventTrigger detail editor.
///
/// Like Schedule, EventTrigger straddles the apply/runtime boundary. The
/// apply path owns the description of the trigger (`trigger_id`,
/// `task_id`, `source_collection`, `event_kind`, `filter`, `enabled`,
/// `concurrency`, `created_at`, `updated_at`); the event-source engine
/// owns the fire bookkeeping (`last_attempt_at`,
/// `last_fired_source_doc_id`, `last_status`, `last_error`,
/// `fire_count`).
///
/// The editor groups those halves visually. `event_kind` is restricted
/// to "created" for PR 2 — the event-source engine only probes the
/// created event today; additional event kinds will land as the engine
/// grows more probe surfaces. The mutation writer in
/// `client/mutations/manage/task.rs` projects only apply-owned fields,
/// so the runtime bookkeeping shown here is never clobbered by a
/// desktop save.
pub(super) fn render_event_trigger_editor(ui: &mut Ui, draft: &mut EventTriggerDraft) {
    editor_heading(ui, "Event Trigger");
    text_field(ui, "Trigger ID", &mut draft.trigger_id);
    text_field(ui, "Task ID", &mut draft.task_id);
    text_field(ui, "Source Collection", &mut draft.source_collection);
    // PR 2 only supports the "created" event kind; the field is kept
    // editable (not locked) so future PRs can introduce more event
    // kinds without reshaping the draft, but today the only valid
    // value is "created". The apply-time validator in
    // `defra-agent-cli` enforces the same constraint.
    text_field(ui, "Event Kind", &mut draft.event_kind);
    multiline_field(ui, "Filter", &mut draft.filter, 6);
    toggle_field(ui, "Enabled", &mut draft.enabled);
    text_field(ui, "Concurrency", &mut draft.concurrency);
    read_only_field(ui, "Created At", draft.created_at.as_str());
    read_only_field(ui, "Updated At", draft.updated_at.as_str());

    ui.add_space(8.0);
    editor_heading(ui, "Runtime State");
    // Event-source-owned fields. Shown read-only so the desktop cannot
    // accidentally reset the engine's bookkeeping by re-applying a
    // trigger edit. The mutation writer enforces the same contract at
    // the GraphQL layer by omitting these fields from upsert input.
    read_only_field(ui, "Last Attempt At", draft.last_attempt_at.as_str());
    read_only_field(ui, "Last Status", draft.last_status.as_str());
    read_only_multiline(ui, "Last Error", draft.last_error.as_str(), 4);
    read_only_field(ui, "Fire Count", draft.fire_count.as_str());
    read_only_field(
        ui,
        "Last Fired Source Doc",
        draft.last_fired_source_doc_id.as_str(),
    );
}
