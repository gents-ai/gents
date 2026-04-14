use anyhow::{bail, Context, Result};
use chrono::Utc;
use defra_agent_protocol::row::{
    AgentBehaviorRow, InferenceBackendRow, InferenceProfileRow, ScheduledTaskRow, ToolSelectionRow,
};
use defra_node::EmbeddedNode;
use uuid::Uuid;

use super::store::ClientStore;

const DEFAULT_REQUEST_MAX_RETRIES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedConversation {
    pub session_id: String,
    pub agent_did: String,
    pub behavior_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedRequest {
    pub request_id: String,
    pub session_id: String,
    pub agent_did: String,
    pub behavior_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerMutationResult {
    pub peer_id: String,
    pub label: String,
    pub addr: String,
    pub connected: bool,
    pub warning: Option<String>,
}

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
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_optional_bool_field(
            "supports_tool_calls",
            row.supports_tool_calls,
        )),
        Some(graphql_optional_bool_field(
            "supports_streaming",
            row.supports_streaming,
        )),
        Some(graphql_optional_bool_field(
            "supports_structured_outputs",
            row.supports_structured_outputs,
        )),
        Some(graphql_optional_bool_field(
            "supports_json_schema",
            row.supports_json_schema,
        )),
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
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_optional_bool_field(
            "supports_tool_calls",
            row.supports_tool_calls,
        )),
        Some(graphql_optional_bool_field(
            "supports_streaming",
            row.supports_streaming,
        )),
        Some(graphql_optional_bool_field(
            "supports_structured_outputs",
            row.supports_structured_outputs,
        )),
        Some(graphql_optional_bool_field(
            "supports_json_schema",
            row.supports_json_schema,
        )),
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

pub async fn create_conversation(
    node: &EmbeddedNode,
    store: &ClientStore,
    agent_did: &str,
    behavior_id: Option<&str>,
) -> Result<CreatedConversation> {
    let agent_did = normalize_required("agent_did", agent_did)?;
    let session_id = Uuid::new_v4().to_string();
    let binding = resolve_agent_binding(store, agent_did, behavior_id, None)?;

    upsert_session(
        node,
        store,
        &session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
    )
    .await?;
    upsert_conversation(
        node,
        store,
        &session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
        "",
        "",
        "active",
    )
    .await?;

    Ok(CreatedConversation {
        session_id,
        agent_did: agent_did.to_string(),
        behavior_id: binding.behavior_id,
    })
}

pub async fn submit_request(
    node: &EmbeddedNode,
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
    content: &str,
    behavior_id: Option<&str>,
) -> Result<SubmittedRequest> {
    let session_id = normalize_required("session_id", session_id)?;
    let agent_did = normalize_required("agent_did", agent_did)?;
    let content = normalize_required("content", content)?;
    let request_id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let binding = resolve_agent_binding(store, agent_did, behavior_id, Some(session_id))?;

    upsert_session(
        node,
        store,
        session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
    )
    .await?;

    let escaped_request_id = escape_graphql_string(&request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_behavior_id = escape_graphql_string(binding.behavior_id.as_deref().unwrap_or(""));
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_content = escape_graphql_string(content);
    let escaped_created_at = escape_graphql_string(&created_at);

    let mutation = format!(
        r#"mutation {{
            add_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_behavior_id}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "{escaped_content}",
                status: "pending",
                lifecycle_state: "pending",
                admission_state: "released",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{escaped_created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = DEFAULT_REQUEST_MAX_RETRIES,
    );
    execute_mutation(node, &mutation, "submit_request").await?;

    upsert_conversation(
        node,
        store,
        session_id,
        agent_did,
        &binding.agent_name,
        &binding.behavior_id,
        &request_id,
        content,
        "active",
    )
    .await?;

    Ok(SubmittedRequest {
        request_id,
        session_id: session_id.to_string(),
        agent_did: agent_did.to_string(),
        behavior_id: binding.behavior_id,
    })
}

struct ResolvedAgentBinding {
    agent_name: String,
    behavior_id: Option<String>,
}

fn resolve_agent_binding(
    store: &ClientStore,
    agent_did: &str,
    requested_behavior_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<ResolvedAgentBinding> {
    let existing_conversation = session_id.and_then(|session_id| {
        store
            .conversations
            .iter()
            .find(|row| row.session_id == session_id)
    });
    let existing_session = session_id.and_then(|session_id| {
        store
            .sessions
            .iter()
            .find(|row| row.session_id == session_id)
    });

    let behavior_id = resolve_behavior_id(
        store,
        agent_did,
        requested_behavior_id,
        existing_conversation.and_then(|row| row.behavior_id.as_deref()),
        existing_session.and_then(|row| row.behavior_id.as_deref()),
    )?;
    let agent_name = existing_conversation
        .and_then(|row| normalize_optional_string(row.agent_name.as_deref()))
        .or_else(|| {
            existing_session.and_then(|row| normalize_optional_string(row.agent_name.as_deref()))
        })
        .or_else(|| {
            store
                .agent_principals
                .iter()
                .find(|row| row.agent_did == agent_did)
                .and_then(|row| normalize_optional_string(row.display_name.as_deref()))
        })
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_display_name_for_did(agent_did));

    Ok(ResolvedAgentBinding {
        agent_name,
        behavior_id,
    })
}

fn resolve_behavior_id(
    store: &ClientStore,
    agent_did: &str,
    requested_behavior_id: Option<&str>,
    existing_conversation_behavior_id: Option<&str>,
    existing_session_behavior_id: Option<&str>,
) -> Result<Option<String>> {
    let requested = normalize_optional_string(requested_behavior_id);

    let conversation_behavior = normalize_optional_string(existing_conversation_behavior_id);
    let session_behavior = normalize_optional_string(existing_session_behavior_id);

    if let (Some(existing), Some(requested)) = (conversation_behavior, requested) {
        if existing != requested {
            bail!(
                "AgentConversation session behavior mismatch: existing={existing} requested={requested}"
            );
        }
    }

    if let (Some(existing), Some(requested)) = (session_behavior, requested) {
        if existing != requested {
            bail!(
                "AgentSession session behavior mismatch: existing={existing} requested={requested}"
            );
        }
    }

    let resolved = conversation_behavior
        .or(session_behavior)
        .or(requested)
        .or_else(|| {
            store
                .agent_principals
                .iter()
                .find(|row| row.agent_did == agent_did)
                .and_then(|row| normalize_optional_string(row.default_behavior_id.as_deref()))
        })
        .or_else(|| {
            store
                .behaviors
                .iter()
                .find(|row| {
                    row.agent_did.as_deref() == Some(agent_did) && row.enabled != Some(false)
                })
                .map(|row| row.behavior_id.as_str())
        })
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_behavior_id_for_agent(agent_did));

    Ok(normalize_optional_string(Some(&resolved)).map(ToOwned::to_owned))
}

async fn upsert_session(
    node: &EmbeddedNode,
    store: &ClientStore,
    session_id: &str,
    _agent_did: &str,
    agent_name: &str,
    behavior_id: &Option<String>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let existing = store
        .sessions
        .iter()
        .find(|row| row.session_id == session_id);
    let started = existing
        .and_then(|row| normalize_optional_string(row.started.as_deref()))
        .unwrap_or(now.as_str());

    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_name = escape_graphql_string(agent_name);
    let escaped_behavior_id = escape_graphql_string(behavior_id.as_deref().unwrap_or(""));
    let escaped_started = escape_graphql_string(started);
    let mutation = format!(
        r#"mutation {{
            upsert_AgentSession(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    agent_name: "{escaped_agent_name}",
                    behavior_id: "{escaped_behavior_id}",
                    started: "{escaped_started}",
                    status: "active"
                }},
                update: {{
                    agent_name: "{escaped_agent_name}",
                    behavior_id: "{escaped_behavior_id}",
                    started: "{escaped_started}",
                    status: "active"
                }}
            ) {{ _docID }}
        }}"#
    );
    execute_mutation(node, &mutation, "upsert_session").await
}

#[allow(clippy::too_many_arguments)]
async fn upsert_conversation(
    node: &EmbeddedNode,
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
    agent_name: &str,
    behavior_id: &Option<String>,
    latest_request_id: &str,
    content: &str,
    status: &str,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let existing = store
        .conversations
        .iter()
        .find(|row| row.session_id == session_id);

    let title = existing
        .and_then(|row| normalize_optional_string(row.title.as_deref()))
        .filter(|title| *title != "New Conversation")
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| derive_conversation_title(content));
    let preview_text = if content.is_empty() {
        existing
            .and_then(|row| row.preview_text.as_deref())
            .unwrap_or_default()
            .to_string()
    } else {
        derive_conversation_preview(content)
    };
    let created_at = existing
        .and_then(|row| normalize_optional_string(row.created_at.as_deref()))
        .unwrap_or(now.as_str());
    let latest_request_id = normalize_optional_string(Some(latest_request_id))
        .or_else(|| {
            existing.and_then(|row| normalize_optional_string(row.latest_request_id.as_deref()))
        })
        .unwrap_or_default();

    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_name = escape_graphql_string(agent_name);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_behavior_id = escape_graphql_string(behavior_id.as_deref().unwrap_or(""));
    let escaped_title = escape_graphql_string(&title);
    let escaped_preview = escape_graphql_string(&preview_text);
    let escaped_status = escape_graphql_string(status);
    let escaped_created_at = escape_graphql_string(created_at);
    let escaped_latest_request_id = escape_graphql_string(latest_request_id);
    let mutation = format!(
        r#"mutation {{
            upsert_AgentConversation(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    agent_name: "{escaped_agent_name}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    title: "{escaped_title}",
                    preview_text: "{escaped_preview}",
                    status: "{escaped_status}",
                    created_at: "{escaped_created_at}",
                    updated_at: "{now}",
                    latest_request_id: "{escaped_latest_request_id}"
                }},
                update: {{
                    agent_name: "{escaped_agent_name}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    title: "{escaped_title}",
                    preview_text: "{escaped_preview}",
                    status: "{escaped_status}",
                    created_at: "{escaped_created_at}",
                    updated_at: "{now}",
                    latest_request_id: "{escaped_latest_request_id}"
                }}
            ) {{ _docID }}
        }}"#
    );
    execute_mutation(node, &mutation, "upsert_conversation").await
}

async fn execute_mutation(node: &EmbeddedNode, mutation: &str, operation: &str) -> Result<()> {
    let response = node.execute(mutation).await;
    if response.has_errors() {
        bail!(
            "{operation} failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(())
}

fn join_fields(fields: &[Option<String>]) -> String {
    fields
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(",\n                    ")
}

fn graphql_string_field(name: &str, value: Option<&str>) -> String {
    match normalize_optional_string(value) {
        Some(value) => format!(r#"{name}: "{}""#, escape_graphql_string(value)),
        None => format!("{name}: null"),
    }
}

fn graphql_string_list_field(name: &str, values: &[String]) -> String {
    let values = values
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}: [{values}]")
}

fn graphql_optional_bool_field(name: &str, value: Option<bool>) -> String {
    match value {
        Some(value) => format!("{name}: {value}"),
        None => format!("{name}: null"),
    }
}

fn graphql_optional_int_field(name: &str, value: Option<i64>) -> String {
    match value {
        Some(value) => format!("{name}: {value}"),
        None => format!("{name}: null"),
    }
}

fn graphql_optional_float_field(name: &str, value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{name}: {value}"),
        None => format!("{name}: null"),
    }
}

fn normalize_required<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    normalize_optional_string(Some(value)).with_context(|| format!("{field} must not be empty"))
}

fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn default_behavior_id_for_agent(agent_did: &str) -> String {
    format!("{agent_did}:default")
}

fn default_display_name_for_did(agent_did: &str) -> String {
    agent_did
        .rsplit(':')
        .next()
        .filter(|segment| !segment.trim().is_empty())
        .unwrap_or(agent_did)
        .to_string()
}

fn derive_conversation_title(content: &str) -> String {
    let normalized = normalize_conversation_text(content);
    if normalized.is_empty() {
        "New Conversation".to_string()
    } else {
        truncate_chars(&normalized, 80)
    }
}

fn derive_conversation_preview(content: &str) -> String {
    truncate_chars(&normalize_conversation_text(content), 240)
}

fn normalize_conversation_text(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn escape_graphql_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_behavior_id_uses_agent_did_suffix() {
        assert_eq!(
            default_behavior_id_for_agent("did:defra:test"),
            "did:defra:test:default".to_string()
        );
    }

    #[test]
    fn conversation_title_defaults_for_empty_content() {
        assert_eq!(derive_conversation_title(""), "New Conversation");
    }
}
