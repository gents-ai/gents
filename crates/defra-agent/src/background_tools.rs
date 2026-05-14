#![allow(dead_code)] // R4b lands these helpers one task ahead of their tool integrations.

pub(crate) mod r4c_args;
mod transcript_render;

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use defra_agent_protocol::transcript::decode_persisted_message;
use defra_node::EmbeddedNode;
use rig::completion::message::{AssistantContent, Message, Text};
use serde::Deserialize;
use serde_json::json;

use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::session::execute_mutation_with_retry;
use crate::tool_call_lifecycle::{AwaitMode, ChildTerminal, FailureClass};

use self::r4c_args::{
    ListStatusFilter, ListSubagentsArgs, ListSubagentsEntry, ListSubagentsResponse,
};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SpawnSubagentArgs {
    pub behavior_id: String,
    pub prompt: String,
    #[serde(default)]
    pub await_mode: AwaitModeArg,
    #[serde(default)]
    pub deadline: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WaitSubagentArgs {
    pub child_request_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CancelSubagentArgs {
    pub child_request_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BackgroundToolArgs {
    pub tool_name: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WaitToolArgs {
    pub tool_call_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CancelToolArgs {
    pub tool_call_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AwaitModeArg {
    #[default]
    Foreground,
    Background,
}

impl AwaitModeArg {
    pub(crate) fn as_await_mode(self) -> AwaitMode {
        match self {
            Self::Foreground => AwaitMode::Foreground,
            Self::Background => AwaitMode::Background,
        }
    }
}

impl<'de> Deserialize<'de> for AwaitModeArg {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.trim() {
            "foreground" => Ok(Self::Foreground),
            "background" => Ok(Self::Background),
            other => Err(serde::de::Error::custom(format!(
                "unsupported await_mode '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParentSubagentContext {
    pub session_id: String,
    pub request_id: String,
    pub behavior_id: String,
    pub subagent_depth: u32,
    pub request_deadline_at: DateTime<Utc>,
    pub allowed_targets: Vec<String>,
    pub subagent_spawn_enabled: bool,
    pub subagent_background_enabled: bool,
    pub cross_deployment_spawn_timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParentSubagentAuthorization {
    pub behavior_id: String,
    pub allowed_targets: Vec<String>,
    pub spawn_enabled: bool,
    pub background_enabled: bool,
    pub cross_deployment_spawn_timeout_seconds: Option<u32>,
}

impl ParentSubagentAuthorization {
    pub(crate) fn authorizes_target(&self, target_behavior_id: &str) -> bool {
        self.allowed_targets
            .iter()
            .any(|target| target == target_behavior_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubagentAuthorizationDenial {
    pub path: &'static str,
    pub requested: String,
    pub message: String,
}

pub(crate) fn subagent_spawn_denial(
    authorization: &ParentSubagentAuthorization,
    target_behavior_id: &str,
    await_mode: AwaitMode,
    tool_name: &str,
) -> Option<SubagentAuthorizationDenial> {
    if !authorization.spawn_enabled {
        return Some(SubagentAuthorizationDenial {
            path: "/",
            requested: tool_name.to_string(),
            message: "subagent spawning is not enabled for this behavior".to_string(),
        });
    }

    if await_mode == AwaitMode::Background && !authorization.background_enabled {
        return Some(SubagentAuthorizationDenial {
            path: "/await_mode",
            requested: "background".to_string(),
            message: "background subagent spawning is not enabled for this behavior".to_string(),
        });
    }

    if !authorization.authorizes_target(target_behavior_id) {
        return Some(SubagentAuthorizationDenial {
            path: "/behavior_id",
            requested: target_behavior_id.to_string(),
            message: format!(
                "behavior '{target_behavior_id}' is not allowed as a subagent target for this behavior"
            ),
        });
    }

    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildEdge {
    pub parent_tool_call_id: String,
    pub child_request_id: String,
    pub child_session_id: String,
    pub behavior_id: String,
    pub await_mode: AwaitMode,
    pub lifecycle_state: String,
}

#[derive(Debug, Deserialize)]
struct ParentRequestRow {
    request_id: String,
    session_id: String,
    behavior_id: Option<String>,
    subagent_depth: Option<u32>,
    deadline: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ParentAuthorizationRequestRow {
    behavior_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AgentBehaviorToolSelectionRow {
    tool_selection_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ToolSelectionTargetsRow {
    subagent_targets: Option<Vec<String>>,
    subagent_spawn_enabled: Option<bool>,
    subagent_background_enabled: Option<bool>,
    cross_deployment_spawn_timeout_seconds: Option<u32>,
}

pub(crate) const DEFAULT_CROSS_DEPLOYMENT_SPAWN_TIMEOUT_SECONDS: u32 = 60;

#[derive(Debug, Deserialize)]
struct ListSubagentBridgeRow {
    tool_call_id: String,
    child_request_id: Option<String>,
    lifecycle_state: Option<String>,
    await_mode: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListSubagentChildRow {
    request_id: String,
    session_id: String,
    behavior_id: Option<String>,
    created_at: String,
    subagent_depth: Option<u32>,
}

pub(crate) async fn handle_list_subagents(
    node: &EmbeddedNode,
    caller_request_id: &str,
    local_deployment_id: &str,
    args: ListSubagentsArgs,
) -> Result<ListSubagentsResponse> {
    let limit = args.validated_limit() as usize;
    let escaped_caller = escape_graphql_string(caller_request_id);
    let escaped_spawn_tool = escape_graphql_string("spawn_subagent");
    let bridge_query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    request_id: {{ _eq: "{escaped_caller}" }},
                    tool_name: {{ _eq: "{escaped_spawn_tool}" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                tool_call_id
                child_request_id
                lifecycle_state
                await_mode
                started_at
                completed_at
            }}
        }}"#
    );
    let bridge_response = node.execute(&bridge_query).await;
    if bridge_response.has_errors() {
        anyhow::bail!(
            "list_subagents bridge query failed: {:?}",
            bridge_response.errors
        );
    }
    let bridges: Vec<ListSubagentBridgeRow> = rows(bridge_response.data.as_ref(), "AgentToolCall")?;

    let child_query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    caused_by_parent_request_id: {{ _eq: "{escaped_caller}" }}
                }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                session_id
                behavior_id
                created_at
                subagent_depth
            }}
        }}"#
    );
    let child_response = node.execute(&child_query).await;
    if child_response.has_errors() {
        anyhow::bail!(
            "list_subagents child query failed: {:?}",
            child_response.errors
        );
    }
    let children_by_request =
        rows::<ListSubagentChildRow>(child_response.data.as_ref(), "AgentRequest")?
            .into_iter()
            .map(|row| (row.request_id.clone(), row))
            .collect::<HashMap<_, _>>();

    let mut entries = Vec::new();
    for bridge in bridges {
        if bridge.await_mode.as_deref() != Some("background") {
            continue;
        }
        let Some(child_request_id) = non_empty_string(bridge.child_request_id.as_deref()) else {
            continue;
        };
        let status = bridge
            .lifecycle_state
            .as_deref()
            .filter(|state| !state.trim().is_empty())
            .unwrap_or("running");
        if !list_subagent_status_matches(args.status, status) {
            continue;
        }
        let Some(child) = children_by_request.get(&child_request_id) else {
            continue;
        };
        let created_at = parse_rfc3339(Some(&child.created_at)).ok_or_else(|| {
            anyhow!("child AgentRequest {child_request_id} has invalid created_at")
        })?;
        let last_update = parse_rfc3339(bridge.completed_at.as_deref())
            .or_else(|| parse_rfc3339(bridge.started_at.as_deref()))
            .unwrap_or(created_at);

        entries.push(ListSubagentsEntry {
            child_request_id,
            child_session_id: child.session_id.clone(),
            behavior_id: non_empty_string(child.behavior_id.as_deref()).unwrap_or_default(),
            deployment_id: local_deployment_id.to_string(),
            await_mode: "background".to_string(),
            status: status.to_string(),
            created_at,
            last_update,
            depth: child.subagent_depth.unwrap_or_default(),
        });
    }

    let truncated = entries.len() > limit;
    entries.truncate(limit);
    Ok(ListSubagentsResponse {
        read_at: Utc::now(),
        truncated,
        entries,
    })
}

fn list_subagent_status_matches(filter: ListStatusFilter, status: &str) -> bool {
    match filter {
        ListStatusFilter::Running => status == "running",
        ListStatusFilter::Terminal => matches!(
            status,
            "completed" | "failed" | "timedOut" | "cancelled" | "dead" | "interrupted"
        ),
        ListStatusFilter::All => !status.trim().is_empty(),
    }
}

pub(crate) async fn load_parent_subagent_context(
    node: &EmbeddedNode,
    parent_request_id: &str,
) -> Result<ParentSubagentContext> {
    let escaped_request_id = escape_graphql_string(parent_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{
                request_id
                session_id
                behavior_id
                subagent_depth
                deadline
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query parent AgentRequest {parent_request_id} failed: {:?}",
            response.errors
        );
    }

    let row: ParentRequestRow = first_row(response.data.as_ref(), "AgentRequest")
        .ok_or_else(|| anyhow!("parent AgentRequest {parent_request_id} not found"))?;
    let behavior_id = row
        .behavior_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("parent AgentRequest {parent_request_id} has no behavior_id"))?;
    let deadline = row
        .deadline
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
        .ok_or_else(|| anyhow!("parent AgentRequest {parent_request_id} has no valid deadline"))?;
    let selection = load_subagent_tool_selection(node, &behavior_id).await?;

    Ok(ParentSubagentContext {
        session_id: row.session_id,
        request_id: row.request_id,
        behavior_id,
        subagent_depth: row.subagent_depth.unwrap_or_default(),
        request_deadline_at: deadline,
        allowed_targets: selection.allowed_targets,
        subagent_spawn_enabled: selection.spawn_enabled,
        subagent_background_enabled: selection.background_enabled,
        cross_deployment_spawn_timeout_seconds: selection.cross_deployment_spawn_timeout_seconds,
    })
}

pub(crate) async fn parent_authorizes_subagent_target(
    node: &EmbeddedNode,
    parent_request_id: &str,
    target_behavior_id: &str,
) -> Result<bool> {
    Ok(load_parent_subagent_authorization(node, parent_request_id)
        .await?
        .authorizes_target(target_behavior_id))
}

pub(crate) async fn load_parent_subagent_authorization(
    node: &EmbeddedNode,
    parent_request_id: &str,
) -> Result<ParentSubagentAuthorization> {
    let escaped_request_id = escape_graphql_string(parent_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{
                behavior_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query parent AgentRequest {parent_request_id} authorization failed: {:?}",
            response.errors
        );
    }

    let row: ParentAuthorizationRequestRow = first_row(response.data.as_ref(), "AgentRequest")
        .ok_or_else(|| anyhow!("parent AgentRequest {parent_request_id} not found"))?;
    let behavior_id = row
        .behavior_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("parent AgentRequest {parent_request_id} has no behavior_id"))?;
    let selection = load_subagent_tool_selection(node, &behavior_id).await?;

    Ok(ParentSubagentAuthorization {
        behavior_id,
        allowed_targets: selection.allowed_targets,
        spawn_enabled: selection.spawn_enabled,
        background_enabled: selection.background_enabled,
        cross_deployment_spawn_timeout_seconds: selection.cross_deployment_spawn_timeout_seconds,
    })
}

pub(crate) fn effective_cross_deployment_spawn_timeout_seconds(
    authorization: &ParentSubagentAuthorization,
) -> u32 {
    authorization
        .cross_deployment_spawn_timeout_seconds
        .unwrap_or(DEFAULT_CROSS_DEPLOYMENT_SPAWN_TIMEOUT_SECONDS)
}

pub(crate) fn effective_context_cross_deployment_spawn_timeout_seconds(
    context: &ParentSubagentContext,
) -> u32 {
    context
        .cross_deployment_spawn_timeout_seconds
        .unwrap_or(DEFAULT_CROSS_DEPLOYMENT_SPAWN_TIMEOUT_SECONDS)
}

pub(crate) fn target_is_allowed(context: &ParentSubagentContext, target_behavior_id: &str) -> bool {
    context
        .allowed_targets
        .iter()
        .any(|target| target == target_behavior_id)
}

struct SubagentToolSelection {
    allowed_targets: Vec<String>,
    spawn_enabled: bool,
    background_enabled: bool,
    cross_deployment_spawn_timeout_seconds: Option<u32>,
}

async fn load_subagent_tool_selection(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<SubagentToolSelection> {
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let behavior_query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{escaped_behavior_id}" }} }},
                limit: 1
            ) {{
                tool_selection_id
            }}
        }}"#
    );
    let behavior_response = node.execute(&behavior_query).await;
    if behavior_response.has_errors() {
        anyhow::bail!(
            "query AgentBehavior {behavior_id} for subagent targets failed: {:?}",
            behavior_response.errors
        );
    }
    let behavior: AgentBehaviorToolSelectionRow =
        first_row(behavior_response.data.as_ref(), "AgentBehavior")
            .ok_or_else(|| anyhow!("AgentBehavior {behavior_id} not found"))?;
    let selection_id = match behavior
        .tool_selection_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(selection_id) => selection_id,
        None => {
            return Ok(SubagentToolSelection {
                allowed_targets: Vec::new(),
                spawn_enabled: false,
                background_enabled: false,
                cross_deployment_spawn_timeout_seconds: None,
            });
        }
    };

    let escaped_selection_id = escape_graphql_string(selection_id);
    let selection_query = format!(
        r#"{{
            ToolSelection(
                filter: {{ selection_id: {{ _eq: "{escaped_selection_id}" }} }},
                limit: 1
            ) {{
                subagent_targets
                subagent_spawn_enabled
                subagent_background_enabled
                cross_deployment_spawn_timeout_seconds
            }}
        }}"#
    );
    let selection_response = node.execute(&selection_query).await;
    if selection_response.has_errors() {
        anyhow::bail!(
            "query ToolSelection {selection_id} for subagent targets failed: {:?}",
            selection_response.errors
        );
    }
    let Some(selection) =
        first_row::<ToolSelectionTargetsRow>(selection_response.data.as_ref(), "ToolSelection")
    else {
        return Ok(SubagentToolSelection {
            allowed_targets: Vec::new(),
            spawn_enabled: false,
            background_enabled: false,
            cross_deployment_spawn_timeout_seconds: None,
        });
    };

    Ok(SubagentToolSelection {
        allowed_targets: dedupe_non_empty(selection.subagent_targets.unwrap_or_default()),
        spawn_enabled: selection.subagent_spawn_enabled.unwrap_or(false),
        background_enabled: selection.subagent_background_enabled.unwrap_or(false),
        cross_deployment_spawn_timeout_seconds: selection.cross_deployment_spawn_timeout_seconds,
    })
}

#[cfg(test)]
mod cross_deployment_timeout_tests {
    use super::*;

    fn auth(timeout: Option<u32>) -> ParentSubagentAuthorization {
        ParentSubagentAuthorization {
            behavior_id: "parent".to_string(),
            allowed_targets: vec!["child".to_string()],
            spawn_enabled: true,
            background_enabled: true,
            cross_deployment_spawn_timeout_seconds: timeout,
        }
    }

    #[test]
    fn override_takes_precedence() {
        assert_eq!(
            effective_cross_deployment_spawn_timeout_seconds(&auth(Some(120))),
            120
        );
    }

    #[test]
    fn default_when_none() {
        assert_eq!(
            effective_cross_deployment_spawn_timeout_seconds(&auth(None)),
            DEFAULT_CROSS_DEPLOYMENT_SPAWN_TIMEOUT_SECONDS
        );
    }
}

#[derive(Debug, Deserialize)]
struct ChildRequestEdgeRow {
    request_id: String,
    session_id: String,
    behavior_id: Option<String>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ParentToolCallEdgeRow {
    tool_call_id: String,
    request_id: Option<String>,
    lifecycle_state: Option<String>,
    await_mode: Option<String>,
    child_request_id: Option<String>,
}

pub(crate) async fn load_authorized_child_edge(
    node: &EmbeddedNode,
    parent_context: &ParentSubagentContext,
    child_request_id: &str,
) -> Result<ChildEdge> {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let child_query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{
                request_id
                session_id
                behavior_id
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#
    );
    let child_response = node.execute(&child_query).await;
    if child_response.has_errors() {
        anyhow::bail!(
            "query child AgentRequest {child_request_id} failed: {:?}",
            child_response.errors
        );
    }
    let child: ChildRequestEdgeRow = first_row(child_response.data.as_ref(), "AgentRequest")
        .ok_or_else(|| anyhow!("child AgentRequest {child_request_id} not found"))?;
    if child.caused_by_parent_request_id.as_deref() != Some(parent_context.request_id.as_str()) {
        anyhow::bail!(
            "child AgentRequest {child_request_id} is not linked to parent request {}",
            parent_context.request_id
        );
    }
    let parent_tool_call_id = child
        .caused_by_parent_tool_call_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow!("child AgentRequest {child_request_id} has no parent tool-call link")
        })?;
    let behavior_id = child
        .behavior_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("child AgentRequest {child_request_id} has no behavior_id"))?;
    let escaped_parent_session_id = escape_graphql_string(&parent_context.session_id);
    let escaped_parent_request_id = escape_graphql_string(&parent_context.request_id);
    let escaped_parent_tool_call_id = escape_graphql_string(&parent_tool_call_id);
    let tool_call_query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_parent_session_id}" }},
                    request_id: {{ _eq: "{escaped_parent_request_id}" }},
                    tool_call_id: {{ _eq: "{escaped_parent_tool_call_id}" }}
                }},
                limit: 1
            ) {{
                tool_call_id
                request_id
                lifecycle_state
                await_mode
                child_request_id
            }}
        }}"#
    );
    let tool_call_response = node.execute(&tool_call_query).await;
    if tool_call_response.has_errors() {
        anyhow::bail!(
            "query parent AgentToolCall {parent_tool_call_id} failed: {:?}",
            tool_call_response.errors
        );
    }
    let tool_call: ParentToolCallEdgeRow =
        first_row(tool_call_response.data.as_ref(), "AgentToolCall").ok_or_else(|| {
            anyhow!(
                "parent AgentToolCall {parent_tool_call_id} not found for child {child_request_id}"
            )
        })?;
    if tool_call.request_id.as_deref() != Some(parent_context.request_id.as_str()) {
        anyhow::bail!(
            "parent AgentToolCall {parent_tool_call_id} is not linked to parent request {}",
            parent_context.request_id
        );
    }
    if tool_call.child_request_id.as_deref() != Some(child.request_id.as_str()) {
        anyhow::bail!(
            "parent AgentToolCall {parent_tool_call_id} does not point at child {child_request_id}"
        );
    }
    let await_mode = tool_call
        .await_mode
        .as_deref()
        .and_then(AwaitMode::from_persisted)
        .unwrap_or(AwaitMode::Foreground);

    Ok(ChildEdge {
        parent_tool_call_id: tool_call.tool_call_id,
        child_request_id: child.request_id,
        child_session_id: child.session_id,
        behavior_id,
        await_mode,
        lifecycle_state: tool_call
            .lifecycle_state
            .unwrap_or_else(|| "running".to_string()),
    })
}

#[derive(Debug, Deserialize)]
struct AgentResponseFinalRow {
    materialized_message_sequence: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AgentMessageContentRow {
    role: String,
    content: String,
}

pub(crate) async fn load_child_final_response(
    node: &EmbeddedNode,
    child_edge: &ChildEdge,
) -> Result<Option<String>> {
    let child_request_id = &child_edge.child_request_id;
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let response_query = format!(
        r#"{{
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                materialized_message_sequence
            }}
        }}"#
    );
    let response = node.execute(&response_query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query child AgentResponse {child_request_id} failed: {:?}",
            response.errors
        );
    }
    let Some(response_row) =
        first_row::<AgentResponseFinalRow>(response.data.as_ref(), "AgentResponse")
    else {
        return Ok(None);
    };
    let Some(sequence) = response_row.materialized_message_sequence else {
        return Ok(None);
    };

    let escaped_session_id = escape_graphql_string(&child_edge.child_session_id);
    let message_query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    sequence: {{ _eq: {sequence} }}
                }},
                limit: 1
            ) {{
                role
                content
            }}
        }}"#
    );
    let message = node.execute(&message_query).await;
    if message.has_errors() {
        anyhow::bail!(
            "query child AgentMessage {child_request_id} sequence {sequence} failed: {:?}",
            message.errors
        );
    }
    let Some(message_row) =
        first_row::<AgentMessageContentRow>(message.data.as_ref(), "AgentMessage")
    else {
        return Ok(None);
    };
    if message_row.role != "assistant" {
        anyhow::bail!(
            "materialized child response {child_request_id} sequence {sequence} is role {}",
            message_row.role
        );
    }

    Ok(Some(render_assistant_message_text(&message_row.content)?))
}

pub(crate) async fn load_child_session_id(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Result<Option<String>> {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{
                session_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query child AgentRequest {child_request_id} session failed: {:?}",
            response.errors
        );
    }
    #[derive(Deserialize)]
    struct SessionRow {
        session_id: String,
    }
    Ok(first_row::<SessionRow>(response.data.as_ref(), "AgentRequest").map(|row| row.session_id))
}

pub(crate) async fn load_child_terminal_row(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Result<Option<ChildRequestTerminalRow>> {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{
                status
                lifecycle_state
                failure_reason
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query child AgentRequest {child_request_id} terminal state failed: {:?}",
            response.errors
        );
    }
    Ok(first_row::<ChildRequestTerminalRow>(
        response.data.as_ref(),
        "AgentRequest",
    ))
}

fn render_assistant_message_text(content: &str) -> Result<String> {
    let message = decode_persisted_message("assistant", content);
    let Message::Assistant { content, .. } = message else {
        anyhow::bail!("materialized child response is not an assistant message");
    };

    let mut parts = Vec::new();
    for item in content.iter() {
        match item {
            AssistantContent::Text(Text { text }) => parts.push(text.clone()),
            other => parts.push(serde_json::to_string(other)?),
        }
    }
    Ok(parts.join("\n"))
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChildRequestTerminalRow {
    pub status: Option<String>,
    pub lifecycle_state: Option<String>,
    pub failure_reason: Option<String>,
}

pub(crate) fn project_child_terminal(row: &ChildRequestTerminalRow) -> Option<ChildTerminal> {
    let lifecycle_state = row.lifecycle_state.as_deref().unwrap_or_default();
    match lifecycle_state {
        "completed" | "complete" => None,
        "failed" | "error" => Some(ChildTerminal::Failed {
            reason: non_empty_string(row.failure_reason.as_deref())
                .unwrap_or_else(|| "child request failed".to_string()),
            failure_class: FailureClass::External,
        }),
        "dead" | "timedOut" => Some(ChildTerminal::Dead),
        "interrupted" | "cancelled" => Some(ChildTerminal::Interrupted),
        "superseded" => Some(ChildTerminal::Superseded),
        _ => match row.status.as_deref().unwrap_or_default() {
            "complete" | "completed" => None,
            "error" | "failed" => Some(ChildTerminal::Failed {
                reason: non_empty_string(row.failure_reason.as_deref())
                    .unwrap_or_else(|| "child request failed".to_string()),
                failure_class: FailureClass::External,
            }),
            "interrupted" | "cancelled" => Some(ChildTerminal::Interrupted),
            "superseded" => Some(ChildTerminal::Superseded),
            _ => None,
        },
    }
}

pub(crate) fn child_request_completed(row: &ChildRequestTerminalRow) -> bool {
    matches!(
        row.lifecycle_state.as_deref(),
        Some("completed" | "complete")
    ) || matches!(row.status.as_deref(), Some("completed" | "complete"))
}

pub(crate) fn subagent_tool_not_allowed_payload(
    tool_name: &str,
    path: &str,
    requested: &str,
    message: impl Into<String>,
    allowed_targets: &[String],
) -> String {
    serde_json::to_string(&json!({
        "ok": false,
        "failure_class": "tool_not_allowed",
        "path": path,
        "message": message.into(),
        "retryable": false,
        "service_id": "subagent",
        "tool_name": tool_name,
        "requested_tool_name": requested,
        "allowed_subagent_targets": allowed_targets
    }))
    .unwrap_or_else(|_| {
        r#"{"ok":false,"failure_class":"tool_not_allowed","service_id":"subagent"}"#.to_string()
    })
}

pub(crate) async fn fail_running_subagent_tool_call(
    node: &EmbeddedNode,
    doc_id: &str,
    started_at: Option<&str>,
    deadline_at: Option<&str>,
    result: &str,
    failure: FailureClass,
) -> Result<bool> {
    let now = Utc::now();
    let started_at = parse_rfc3339(started_at).unwrap_or(now);
    let latency_ms = (now - started_at).num_milliseconds().max(0);
    let escaped_doc_id = escape_graphql_string(doc_id);
    let escaped_result = escape_graphql_string(result);
    let started_at_str = started_at.to_rfc3339();
    let completed_at_str = now.to_rfc3339();
    let failure_class = failure.as_str();
    let deadline_field = parse_rfc3339(deadline_at)
        .map(|deadline| format!(r#", deadline_at: "{}""#, deadline.to_rfc3339()))
        .unwrap_or_default();

    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{
                    _docID: {{ _eq: "{escaped_doc_id}" }},
                    lifecycle_state: {{ _eq: "running" }}
                }},
                input: {{
                    result: "{escaped_result}",
                    status: "completed",
                    lifecycle_state: "failed",
                    started_at: "{started_at_str}"{deadline_field},
                    completed_at: "{completed_at_str}",
                    tool_failure_class: "{failure_class}",
                    latency_ms: {latency_ms}
                }}
            ) {{ _docID }}
        }}"#
    );

    let response =
        execute_mutation_with_retry(node, &mutation, "fail_running_subagent_tool_call").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_AgentToolCall"))
        .is_some_and(response_has_documents))
}

fn parse_rfc3339(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .filter(|value| !value.trim().is_empty())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn first_row<T>(data: Option<&serde_json::Value>, collection: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    data.and_then(|data| data.get(collection))
        .and_then(|value| serde_json::from_value::<Vec<T>>(value.clone()).ok())
        .and_then(|mut rows| rows.pop())
}

fn rows<T>(data: Option<&serde_json::Value>, collection: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(value) = data.and_then(|data| data.get(collection)) else {
        anyhow::bail!("{collection} field missing from query response");
    };
    serde_json::from_value(value.clone()).map_err(|error| anyhow!("parse {collection}: {error}"))
}

fn dedupe_non_empty(values: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::with_capacity(values.len());
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !deduped.iter().any(|existing| existing == value) {
            deduped.push(value.to_string());
        }
    }
    deduped
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::completion::message::{AssistantContent, Text};
    use rig::one_or_many::OneOrMany;

    #[test]
    fn project_child_terminal_maps_child_states() {
        assert_eq!(
            project_child_terminal(&ChildRequestTerminalRow {
                status: Some("error".to_string()),
                lifecycle_state: Some("failed".to_string()),
                failure_reason: Some("bad output".to_string()),
            }),
            Some(ChildTerminal::Failed {
                reason: "bad output".to_string(),
                failure_class: FailureClass::External,
            })
        );
        assert_eq!(
            project_child_terminal(&ChildRequestTerminalRow {
                status: Some("processing".to_string()),
                lifecycle_state: Some("dead".to_string()),
                failure_reason: None,
            }),
            Some(ChildTerminal::Dead)
        );
        assert_eq!(
            project_child_terminal(&ChildRequestTerminalRow {
                status: Some("interrupted".to_string()),
                lifecycle_state: Some("interrupted".to_string()),
                failure_reason: None,
            }),
            Some(ChildTerminal::Interrupted)
        );
        assert_eq!(
            project_child_terminal(&ChildRequestTerminalRow {
                status: Some("superseded".to_string()),
                lifecycle_state: Some("superseded".to_string()),
                failure_reason: None,
            }),
            Some(ChildTerminal::Superseded)
        );
        assert_eq!(
            project_child_terminal(&ChildRequestTerminalRow {
                status: Some("complete".to_string()),
                lifecycle_state: Some("completed".to_string()),
                failure_reason: None,
            }),
            None
        );
    }

    #[test]
    fn render_assistant_message_text_uses_persisted_assistant_message() {
        let message = Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: "child final answer".to_string(),
            })),
        };
        let content = serde_json::to_string(&message).unwrap();
        assert_eq!(
            render_assistant_message_text(&content).unwrap(),
            "child final answer"
        );
    }

    #[test]
    fn render_assistant_message_text_uses_legacy_assistant_content() {
        let content = OneOrMany::one(AssistantContent::Text(Text {
            text: "legacy child final answer".to_string(),
        }));
        let persisted = serde_json::to_string(&content).unwrap();
        assert_eq!(
            render_assistant_message_text(&persisted).unwrap(),
            "legacy child final answer"
        );
    }

    #[test]
    fn render_assistant_message_text_uses_plain_text_assistant_content() {
        assert_eq!(
            render_assistant_message_text("plain child final answer").unwrap(),
            "plain child final answer"
        );
    }

    #[test]
    fn dedupe_non_empty_trims_and_preserves_order() {
        assert_eq!(
            dedupe_non_empty(vec![
                " alpha ".to_string(),
                "".to_string(),
                "alpha".to_string(),
                "beta".to_string(),
            ]),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }
}
