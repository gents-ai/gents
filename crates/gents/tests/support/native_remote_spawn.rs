//! Read-only observations for the real cross-principal `spawn_subagent` seam.

use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct RemoteSpawnBridge {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub request_id: String,
    pub request_doc_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub lifecycle_state: Option<String>,
    pub child_request_id: Option<String>,
    pub spawn_target_did: Option<String>,
    pub spawn_behavior_id: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub delegated_workspace: Option<serde_json::Value>,
    pub delegated_input: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RemoteSpawnChild {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub request_id: String,
    pub content: String,
    pub agent_did: Option<String>,
    pub requester_did: Option<String>,
    pub behavior_id: Option<String>,
    pub subagent_depth: Option<u32>,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_request_doc_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
    pub caused_by_parent_tool_call_doc_id: Option<String>,
    pub caused_by_trigger_kind: Option<String>,
}

fn exactly_one<T: serde::de::DeserializeOwned>(
    response: &gents::defra_node::QueryResponse,
    collection: &str,
) -> Option<T> {
    assert!(
        !response.has_errors(),
        "{collection} query failed: {:?}",
        response.errors
    );
    let rows = response.data.as_ref()?.get(collection)?.as_array()?;
    assert!(
        rows.len() <= 1,
        "{collection} logical identity resolved to twins: {rows:?}"
    );
    rows.first()
        .cloned()
        .map(|row| serde_json::from_value(row).expect("decode unique canonical row"))
}

pub async fn wait_for_bridge(
    node: &EmbeddedNode,
    session_id: &str,
    provider_call_id: &str,
) -> RemoteSpawnBridge {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let session = escape_graphql_string(session_id);
        let call = escape_graphql_string(provider_call_id);
        let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }}, tool_call_id: {{ _eq: "{call}" }} }}, limit: 2) {{
            _docID request_id request_doc_id tool_call_id tool_name lifecycle_state child_request_id
            spawn_target_did spawn_behavior_id await_mode cancel_policy delegated_workspace delegated_input
        }} }}"#)).await;
        if let Some(row) = exactly_one(&response, "AgentToolCall") {
            return row;
        }
        if tokio::time::Instant::now() >= deadline {
            let evidence = node.execute(&format!(r#"{{
                AgentRequest(filter: {{ session_id: {{ _eq: "{session}" }} }}, limit: 4) {{
                    _docID request_id lifecycle_state failure_reason subagent_depth
                }}
                AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }} }}, limit: 8) {{
                    _docID request_id tool_call_id lifecycle_state tool_failure_class child_request_id
                }}
            }}"#)).await;
            panic!(
                "remote spawn bridge {provider_call_id} was not observed in session {session_id}: data={:?}, errors={:?}",
                evidence.data, evidence.errors
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn wait_for_child(node: &EmbeddedNode, request_id: &str) -> RemoteSpawnChild {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let request = escape_graphql_string(request_id);
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request}" }} }}, limit: 2) {{
            _docID request_id content agent_did requester_did behavior_id subagent_depth
            caused_by_parent_request_id caused_by_parent_request_doc_id
            caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id caused_by_trigger_kind
        }} }}"#
            ))
            .await;
        if let Some(row) = exactly_one(&response, "AgentRequest") {
            return row;
        }
        if tokio::time::Instant::now() >= deadline {
            // Bounded lifecycle evidence only: never dump provider payloads,
            // prompts, tool arguments, or credentials into failure logs.
            let evidence = node
                .execute(&format!(
                    r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{request}" }} }}, limit: 4) {{
                    _docID request_id lifecycle_state failure_reason subagent_depth
                    caused_by_parent_tool_call_id
                }}
                AgentToolCall(filter: {{ child_request_id: {{ _eq: "{request}" }} }}, limit: 4) {{
                    _docID request_id tool_call_id lifecycle_state tool_failure_class
                }}
            }}"#
                ))
                .await;
            panic!(
                "remote child request {request_id} was not materialized: data={:?}, errors={:?}",
                evidence.data, evidence.errors
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
