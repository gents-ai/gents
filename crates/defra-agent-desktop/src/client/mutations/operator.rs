use anyhow::{bail, Context, Result};
use chrono::Utc;
use defra_agent_protocol::row::{
    AgentBehaviorRow, InferenceBackendRow, InferenceProfileRow, ScheduledTaskRow, ToolSelectionRow,
};
use defra_node::EmbeddedNode;

use super::graphql::{
    escape_graphql_string, execute_mutation, graphql_optional_bool_field,
    graphql_optional_float_field, graphql_optional_int_field, graphql_string_field,
    graphql_string_list_field, join_fields, normalize_required,
};

pub async fn upsert_agent_behavior(node: &EmbeddedNode, row: &AgentBehaviorRow) -> Result<()> {
    let behavior_id = normalize_required("behavior_id", &row.behavior_id)?;
    let agent_did = normalize_required(
        "agent_did",
        row.agent_did
            .as_deref()
            .context("agent_did is required for AgentBehavior")?,
    )?;
    let created_at = row
        .created_at
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Utc::now().to_rfc3339());

    let add_fields = [
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(behavior_id)
        )),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "system_prompt",
            row.system_prompt.as_deref(),
        )),
        Some(graphql_string_field(
            "backend_id",
            row.backend_id.as_deref(),
        )),
        Some(graphql_string_field(
            "model_name",
            row.model_name.as_deref(),
        )),
        Some(graphql_string_field(
            "tool_selection_id",
            row.tool_selection_id.as_deref(),
        )),
        Some(graphql_string_field(
            "inference_profile_id",
            row.inference_profile_id.as_deref(),
        )),
        Some(graphql_string_field(
            "compaction_strategy",
            row.compaction_strategy.as_deref(),
        )),
        Some(graphql_optional_float_field(
            "compaction_threshold",
            row.compaction_threshold,
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(format!(
            r#"created_at: "{}""#,
            escape_graphql_string(&created_at)
        )),
    ];
    let update_fields = [
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "system_prompt",
            row.system_prompt.as_deref(),
        )),
        Some(graphql_string_field(
            "backend_id",
            row.backend_id.as_deref(),
        )),
        Some(graphql_string_field(
            "model_name",
            row.model_name.as_deref(),
        )),
        Some(graphql_string_field(
            "tool_selection_id",
            row.tool_selection_id.as_deref(),
        )),
        Some(graphql_string_field(
            "inference_profile_id",
            row.inference_profile_id.as_deref(),
        )),
        Some(graphql_string_field(
            "compaction_strategy",
            row.compaction_strategy.as_deref(),
        )),
        Some(graphql_optional_float_field(
            "compaction_threshold",
            row.compaction_threshold,
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{behavior_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        behavior_id = escape_graphql_string(behavior_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_agent_behavior").await
}

pub async fn upsert_tool_selection(node: &EmbeddedNode, row: &ToolSelectionRow) -> Result<()> {
    let selection_id = normalize_required("selection_id", &row.selection_id)?;
    let agent_did = normalize_required(
        "agent_did",
        row.agent_did
            .as_deref()
            .context("agent_did is required for ToolSelection")?,
    )?;

    let add_fields = [
        Some(format!(
            r#"selection_id: "{}""#,
            escape_graphql_string(selection_id)
        )),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_optional_bool_field(
            "enable_file_tools",
            row.enable_file_tools,
        )),
        Some(graphql_string_field(
            "file_tools_mode",
            row.file_tools_mode.as_deref(),
        )),
        Some(graphql_optional_bool_field("enable_bash", row.enable_bash)),
        Some(graphql_string_field("bash_mode", row.bash_mode.as_deref())),
        Some(graphql_string_list_field(
            "cli_tool_names",
            &row.cli_tool_names,
        )),
        Some(graphql_optional_bool_field(
            "enable_meta_tools",
            row.enable_meta_tools,
        )),
        Some(graphql_string_list_field("delegate_to", &row.delegate_to)),
    ];
    let update_fields = [
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_optional_bool_field(
            "enable_file_tools",
            row.enable_file_tools,
        )),
        Some(graphql_string_field(
            "file_tools_mode",
            row.file_tools_mode.as_deref(),
        )),
        Some(graphql_optional_bool_field("enable_bash", row.enable_bash)),
        Some(graphql_string_field("bash_mode", row.bash_mode.as_deref())),
        Some(graphql_string_list_field(
            "cli_tool_names",
            &row.cli_tool_names,
        )),
        Some(graphql_optional_bool_field(
            "enable_meta_tools",
            row.enable_meta_tools,
        )),
        Some(graphql_string_list_field("delegate_to", &row.delegate_to)),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_ToolSelection(
                filter: {{ selection_id: {{ _eq: "{selection_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        selection_id = escape_graphql_string(selection_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_tool_selection").await
}

pub async fn upsert_inference_profile(
    node: &EmbeddedNode,
    row: &InferenceProfileRow,
) -> Result<()> {
    let profile_id = normalize_required("profile_id", &row.profile_id)?;

    let add_fields = [
        Some(format!(
            r#"profile_id: "{}""#,
            escape_graphql_string(profile_id)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_optional_int_field(
            "context_window",
            row.context_window,
        )),
        Some(graphql_optional_int_field(
            "max_output_tokens",
            row.max_output_tokens,
        )),
        Some(graphql_optional_int_field("max_turns", row.max_turns)),
        Some(graphql_optional_float_field("temperature", row.temperature)),
        Some(graphql_optional_int_field(
            "stream_batch_ms",
            row.stream_batch_ms,
        )),
        Some(graphql_optional_int_field(
            "deadline_duration_secs",
            row.deadline_duration_secs,
        )),
    ];
    let update_fields = [
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_optional_int_field(
            "context_window",
            row.context_window,
        )),
        Some(graphql_optional_int_field(
            "max_output_tokens",
            row.max_output_tokens,
        )),
        Some(graphql_optional_int_field("max_turns", row.max_turns)),
        Some(graphql_optional_float_field("temperature", row.temperature)),
        Some(graphql_optional_int_field(
            "stream_batch_ms",
            row.stream_batch_ms,
        )),
        Some(graphql_optional_int_field(
            "deadline_duration_secs",
            row.deadline_duration_secs,
        )),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_InferenceProfile(
                filter: {{ profile_id: {{ _eq: "{profile_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        profile_id = escape_graphql_string(profile_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_inference_profile").await
}

pub async fn upsert_inference_backend(
    node: &EmbeddedNode,
    row: &InferenceBackendRow,
) -> Result<()> {
    let backend_id = normalize_required("backend_id", &row.backend_id)?;

    let add_fields = [
        Some(format!(
            r#"backend_id: "{}""#,
            escape_graphql_string(backend_id)
        )),
        Some(graphql_string_field("name", row.name.as_deref())),
        Some(graphql_string_field(
            "provider_kind",
            row.provider_kind.as_deref(),
        )),
        Some(graphql_string_field("endpoint", row.endpoint.as_deref())),
        Some(graphql_string_field("api_key", row.api_key.as_deref())),
        Some(graphql_string_field(
            "api_key_env_var",
            row.api_key_env_var.as_deref(),
        )),
        Some(graphql_optional_int_field(
            "max_concurrent",
            row.max_concurrent,
        )),
        Some(graphql_optional_int_field(
            "max_queue_depth",
            row.max_queue_depth,
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_list_field("models", &row.models)),
        Some(graphql_string_field(
            "last_probe",
            row.last_probe.as_deref(),
        )),
        Some(graphql_string_field(
            "probe_status",
            row.probe_status.as_deref(),
        )),
    ];
    let update_fields = [
        Some(graphql_string_field("name", row.name.as_deref())),
        Some(graphql_string_field(
            "provider_kind",
            row.provider_kind.as_deref(),
        )),
        Some(graphql_string_field("endpoint", row.endpoint.as_deref())),
        Some(graphql_string_field("api_key", row.api_key.as_deref())),
        Some(graphql_string_field(
            "api_key_env_var",
            row.api_key_env_var.as_deref(),
        )),
        Some(graphql_optional_int_field(
            "max_concurrent",
            row.max_concurrent,
        )),
        Some(graphql_optional_int_field(
            "max_queue_depth",
            row.max_queue_depth,
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_list_field("models", &row.models)),
        Some(graphql_string_field(
            "last_probe",
            row.last_probe.as_deref(),
        )),
        Some(graphql_string_field(
            "probe_status",
            row.probe_status.as_deref(),
        )),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{backend_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        backend_id = escape_graphql_string(backend_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_inference_backend").await
}

pub async fn upsert_scheduled_task(node: &EmbeddedNode, row: &ScheduledTaskRow) -> Result<()> {
    let task_id = normalize_required("task_id", &row.task_id)?;
    let agent_did = normalize_required(
        "agent_did",
        row.agent_did
            .as_deref()
            .context("agent_did is required for ScheduledTask")?,
    )?;
    let behavior_id = normalize_required(
        "behavior_id",
        row.behavior_id
            .as_deref()
            .context("behavior_id is required for ScheduledTask")?,
    )?;
    let name = normalize_required(
        "name",
        row.name
            .as_deref()
            .context("name is required for ScheduledTask")?,
    )?;
    let prompt = normalize_required(
        "prompt",
        row.prompt
            .as_deref()
            .context("prompt is required for ScheduledTask")?,
    )?;
    let interval_secs = row
        .interval_secs
        .context("interval_secs is required for ScheduledTask")?;
    if interval_secs <= 0 {
        bail!("interval_secs must be greater than zero");
    }

    let add_fields = [
        Some(format!(r#"task_id: "{}""#, escape_graphql_string(task_id))),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(behavior_id)
        )),
        Some(format!(r#"name: "{}""#, escape_graphql_string(name))),
        Some(format!(r#"prompt: "{}""#, escape_graphql_string(prompt))),
        Some(graphql_optional_int_field(
            "interval_secs",
            Some(interval_secs),
        )),
        Some(graphql_optional_bool_field(
            "enabled",
            Some(row.enabled.unwrap_or(true)),
        )),
        Some(graphql_string_field(
            "next_run_at",
            row.next_run_at.as_deref(),
        )),
        Some(graphql_string_field(
            "last_run_at",
            row.last_run_at.as_deref(),
        )),
        Some(graphql_string_field(
            "last_status",
            row.last_status.as_deref(),
        )),
        Some(graphql_string_field(
            "last_error",
            row.last_error.as_deref(),
        )),
        Some(graphql_optional_int_field("run_count", row.run_count)),
    ];
    let update_fields = [
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(behavior_id)
        )),
        Some(format!(r#"name: "{}""#, escape_graphql_string(name))),
        Some(format!(r#"prompt: "{}""#, escape_graphql_string(prompt))),
        Some(graphql_optional_int_field(
            "interval_secs",
            Some(interval_secs),
        )),
        Some(graphql_optional_bool_field(
            "enabled",
            Some(row.enabled.unwrap_or(true)),
        )),
        Some(graphql_string_field(
            "next_run_at",
            row.next_run_at.as_deref(),
        )),
        Some(graphql_string_field(
            "last_run_at",
            row.last_run_at.as_deref(),
        )),
        Some(graphql_string_field(
            "last_status",
            row.last_status.as_deref(),
        )),
        Some(graphql_string_field(
            "last_error",
            row.last_error.as_deref(),
        )),
        Some(graphql_optional_int_field("run_count", row.run_count)),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_ScheduledTask(
                filter: {{ task_id: {{ _eq: "{task_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        task_id = escape_graphql_string(task_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_scheduled_task").await
}

pub async fn run_scheduled_task_now(node: &EmbeddedNode, row: &ScheduledTaskRow) -> Result<()> {
    if row.enabled != Some(true) {
        bail!("scheduled task must be enabled before it can run now");
    }

    let mut triggered = row.clone();
    triggered.next_run_at = Some(Utc::now().to_rfc3339());
    upsert_scheduled_task(node, &triggered).await
}
