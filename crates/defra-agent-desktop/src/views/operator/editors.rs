use anyhow::Result;
use defra_agent_protocol::row::{
    AgentBehaviorRow, InferenceBackendRow, InferenceProfileRow, ScheduledTaskRow, ToolSelectionRow,
};
use eframe::egui::{self, RichText, TextEdit, Ui};

use crate::audit;
use crate::state::{
    BackendDraft, BehaviorDraft, InferenceProfileDraft, ScheduledTaskDraft, ToolSelectionDraft,
};
use crate::theme;

use super::shared::{
    normalize_optional_owned, normalize_required, parse_optional_f64, parse_optional_i64,
    parse_optional_rfc3339, parse_required_positive_i64, split_csv,
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
    text_field(ui, "API Key", &mut draft.api_key);
    text_field(ui, "API Key Env Var", &mut draft.api_key_env_var);
    text_field(ui, "Max Concurrent", &mut draft.max_concurrent);
    text_field(ui, "Max Queue Depth", &mut draft.max_queue_depth);
    toggle_field(ui, "Enabled", &mut draft.enabled);
    multiline_field(ui, "Models", &mut draft.models, 4);
    text_field(ui, "Probe Status", &mut draft.probe_status);
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

pub(super) fn render_scheduled_task_editor(ui: &mut Ui, draft: &mut ScheduledTaskDraft) {
    editor_heading(ui, "Scheduled Task");
    text_field(ui, "Task ID", &mut draft.task_id);
    text_field(ui, "Agent DID", &mut draft.agent_did);
    text_field(ui, "Behavior ID", &mut draft.behavior_id);
    text_field(ui, "Name", &mut draft.name);
    multiline_field(ui, "Prompt", &mut draft.prompt, 8);
    text_field(ui, "Interval Secs", &mut draft.interval_secs);
    toggle_field(ui, "Enabled", &mut draft.enabled);
    text_field(ui, "Next Run At", &mut draft.next_run_at);
    read_only_field(ui, "Last Run At", draft.last_run_at.as_str());
    read_only_field(ui, "Last Status", draft.last_status.as_str());
    read_only_multiline(ui, "Last Error", draft.last_error.as_str(), 4);
    read_only_field(ui, "Run Count", draft.run_count.as_str());
    read_only_field(ui, "Created At", draft.created_at.as_str());
    read_only_field(ui, "Updated At", draft.updated_at.as_str());
}

pub(super) fn editor_heading(ui: &mut Ui, title: &str) {
    let palette = theme::palette();
    ui.label(
        RichText::new(title)
            .family(theme::stencil_family())
            .size(13.0)
            .color(palette.text_1)
            .strong(),
    );
    ui.add_space(8.0);
}

fn text_field(ui: &mut Ui, label: &str, value: &mut String) {
    let palette = theme::palette();
    let target = audit::targets::operator_field(label);
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    audit::add(
        ui,
        &target,
        TextEdit::singleline(value)
            .id_source(&target)
            .desired_width(ui.available_width()),
    );
    ui.add_space(6.0);
}

fn multiline_field(ui: &mut Ui, label: &str, value: &mut String, rows: usize) {
    let palette = theme::palette();
    let target = audit::targets::operator_field(label);
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    audit::add_sized(
        ui,
        &target,
        [ui.available_width(), rows as f32 * 18.0 + 12.0],
        TextEdit::multiline(value)
            .id_source(&target)
            .desired_rows(rows),
    );
    ui.add_space(6.0);
}

fn toggle_field(ui: &mut Ui, label: &str, value: &mut bool) {
    let target = audit::targets::operator_toggle(label);
    audit::add(ui, &target, egui::Checkbox::new(value, label));
    ui.add_space(6.0);
}

pub(super) fn read_only_field(ui: &mut Ui, label: &str, value: &str) {
    let palette = theme::palette();
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    ui.label(
        RichText::new(if value.trim().is_empty() {
            "unset"
        } else {
            value
        })
        .monospace()
        .size(10.5)
        .color(palette.text_1),
    );
    ui.add_space(6.0);
}

pub(super) fn read_only_multiline(ui: &mut Ui, label: &str, value: &str, rows: usize) {
    let mut value = if value.trim().is_empty() {
        "unset".to_string()
    } else {
        value.to_string()
    };
    let palette = theme::palette();
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    ui.add_sized(
        [ui.available_width(), rows as f32 * 18.0 + 12.0],
        TextEdit::multiline(&mut value)
            .desired_rows(rows)
            .interactive(false),
    );
    ui.add_space(6.0);
}

pub(super) fn behavior_row(draft: &BehaviorDraft) -> Result<AgentBehaviorRow> {
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

pub(super) fn backend_row(draft: &BackendDraft) -> Result<InferenceBackendRow> {
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

pub(super) fn tool_selection_row(draft: &ToolSelectionDraft) -> Result<ToolSelectionRow> {
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

pub(super) fn inference_profile_row(draft: &InferenceProfileDraft) -> Result<InferenceProfileRow> {
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

pub(super) fn scheduled_task_row(draft: &ScheduledTaskDraft) -> Result<ScheduledTaskRow> {
    Ok(ScheduledTaskRow {
        task_id: normalize_required("task_id", &draft.task_id)?.to_string(),
        agent_did: Some(normalize_required("agent_did", &draft.agent_did)?.to_string()),
        behavior_id: Some(normalize_required("behavior_id", &draft.behavior_id)?.to_string()),
        name: Some(normalize_required("name", &draft.name)?.to_string()),
        prompt: Some(normalize_required("prompt", &draft.prompt)?.to_string()),
        interval_secs: Some(parse_required_positive_i64(
            "interval_secs",
            &draft.interval_secs,
        )?),
        enabled: Some(draft.enabled),
        next_run_at: parse_optional_rfc3339("next_run_at", &draft.next_run_at)?,
        last_run_at: parse_optional_rfc3339("last_run_at", &draft.last_run_at)?,
        last_status: normalize_optional_owned(&draft.last_status),
        last_error: normalize_optional_owned(&draft.last_error),
        run_count: parse_optional_i64("run_count", &draft.run_count)?,
        created_at: parse_optional_rfc3339("created_at", &draft.created_at)?,
        updated_at: parse_optional_rfc3339("updated_at", &draft.updated_at)?,
    })
}
