use serde::{Deserialize, Serialize};

use super::escape_graphql_string;
use crate::client_protocol::{
    project_attempt, AttemptView, ClientHeadProjection, ClientTurnState, RequestSnapshot,
};
use crate::output::TranscriptMessage;
use crate::row::{AgentRequestRow, AgentToolCallRow};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphqlSubmittedRequest {
    pub request_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphqlTurnState {
    pub request: Option<AgentRequestRow>,
}

impl GraphqlTurnState {
    pub fn projected_head(&self) -> Option<ClientHeadProjection> {
        self.attempt_view().as_ref().map(project_attempt)
    }

    pub fn derived_turn_state(&self) -> Option<ClientTurnState> {
        self.projected_head().map(|head| head.turn_state)
    }

    pub fn successor_request_id(&self) -> Option<String> {
        self.request
            .as_ref()
            .and_then(|row| clean_optional_string(row.superseded_by_request.as_deref()))
    }

    fn attempt_view(&self) -> Option<AttemptView> {
        let request = self.request.as_ref()?;
        let lifecycle = request.lifecycle_state?;

        Some(AttemptView {
            request: RequestSnapshot {
                request_id: request.request_id.clone(),
                retry_parent_request: clean_optional_string(
                    request.retry_parent_request.as_deref(),
                ),
                lifecycle_state: lifecycle,
                is_superseded: clean_optional_string(request.superseded_by_request.as_deref())
                    .is_some(),
            },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphqlSessionShape {
    pub session_id: String,
    pub request_id: String,
    pub turn_state: Option<String>,
    pub request: Option<AgentRequestRow>,
    /// Headers only. Payload readiness is resolved separately by reconstruction.
    pub messages: Vec<TranscriptMessage>,
    pub tool_calls: Vec<AgentToolCallRow>,
}

pub fn turn_state_query(request_id: &str) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                request_id
                retry_parent_request
                superseded_by_request
                temperature
                top_p
                top_k
                max_tokens
                max_total_tokens
                metadata
                lifecycle_state
                failure_reason
                interrupt_requested_at
                valid_until
            }}
        }}"#
    )
}

pub fn session_shape_query(session_id: &str) -> String {
    let escaped_session_id = escape_graphql_string(session_id);
    format!(
        r#"{{
            AgentMessage(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, order: {{ sequence: ASC }}) {{
                message_key
                session_id
                node_did
                requester_did
                request_doc_id
                publication
                outcome
                sequence
                role
                native_id
                blocks
                created_at
            }}
            AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, order: {{ message_sequence: ASC }}) {{
                tool_call_key
                session_id
                message_sequence
                tool_name
                tool_call_id
                status
                lifecycle_state
                child_request_id
                spawn_target_did
                spawn_agent_id
                await_mode
                deadline_at
                selected_service_id
                selected_tool_name
                tool_failure_class
                denial_reason
                denied_argv
                denied_command
                denied_argument
                denied_subcommand
                denied_prefix
                policy_mode
                policy_network
                cancel_cause
                latency_ms
            }}
        }}"#
    )
}

pub fn parse_turn_state_response(
    value: &serde_json::Value,
) -> serde_json::Result<GraphqlTurnState> {
    let data = value
        .get("data")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let request = data
        .get("AgentRequest")
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    Ok(GraphqlTurnState { request })
}

pub fn parse_session_shape_response(
    session_id: &str,
    request_id: &str,
    turn_state: GraphqlTurnState,
    value: &serde_json::Value,
) -> serde_json::Result<GraphqlSessionShape> {
    let data = value
        .get("data")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let messages = data
        .get("AgentMessage")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let tool_calls = data
        .get("AgentToolCall")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();

    Ok(GraphqlSessionShape {
        session_id: session_id.to_string(),
        request_id: request_id.to_string(),
        turn_state: turn_state
            .derived_turn_state()
            .map(|state| format!("{state:?}")),
        request: turn_state.request,
        messages,
        tool_calls,
    })
}

fn clean_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
