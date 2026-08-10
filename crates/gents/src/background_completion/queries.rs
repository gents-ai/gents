use super::*;

#[derive(Debug, Deserialize)]
pub(super) struct ChildLinkageRow {
    pub(super) caused_by_parent_request_id: Option<String>,
    pub(super) caused_by_parent_request_doc_id: Option<String>,
    pub(super) caused_by_parent_tool_call_id: Option<String>,
    pub(super) caused_by_parent_tool_call_doc_id: Option<String>,
}

pub(super) async fn load_child_linkage(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Result<Option<ChildLinkageRow>> {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query child AgentRequest linkage {child_request_id} failed: {:?}",
            response.errors
        );
    }
    Ok(first_row(response.data.as_ref(), "AgentRequest"))
}

#[derive(Debug, Deserialize)]
pub(super) struct RequestIdRow {
    pub(super) request_id: String,
}

pub(super) async fn load_request_id_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<String>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{ request_id }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query AgentRequest doc {doc_id} failed: {:?}",
            response.errors
        );
    }
    Ok(first_row::<RequestIdRow>(response.data.as_ref(), "AgentRequest").map(|row| row.request_id))
}

#[derive(Debug, Deserialize)]
pub(super) struct TerminalChildRow {
    pub(super) request_id: String,
    pub(super) caused_by_parent_request_id: Option<String>,
    pub(super) caused_by_parent_tool_call_id: Option<String>,
}

pub(super) async fn load_terminal_child_request_ids(node: &EmbeddedNode) -> Result<Vec<String>> {
    let query = r#"{
        AgentRequest(
            filter: {
                lifecycle_state: { _in: ["completed", "failed", "dead", "interrupted", "superseded"] }
            }
        ) {
            request_id
            caused_by_parent_request_id
            caused_by_parent_tool_call_id
        }
    }"#;
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query terminal child AgentRequests failed: {:?}",
            response.errors
        );
    }

    let rows: Vec<TerminalChildRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .filter(|row| {
            non_empty(row.caused_by_parent_request_id.as_deref()).is_some()
                && non_empty(row.caused_by_parent_tool_call_id.as_deref()).is_some()
        })
        .map(|row| row.request_id)
        .collect())
}

#[derive(Debug, Deserialize)]
pub(super) struct AgentRequestQueueRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    pub(super) request_id: String,
    pub(super) agent_did: String,
    pub(super) requester_did: Option<String>,
    pub(super) behavior_id: Option<String>,
    pub(super) session_id: String,
    pub(super) content: String,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) top_k: Option<i64>,
    pub(super) max_tokens: Option<i64>,
    pub(super) metadata: Option<String>,
    pub(super) execution_origin: Option<String>,
    pub(super) created_at: String,
    pub(super) deadline: Option<String>,
    pub(super) subagent_depth: Option<u32>,
    pub(super) caused_by_parent_request_id: Option<String>,
    pub(super) caused_by_parent_request_doc_id: Option<String>,
    pub(super) caused_by_parent_tool_call_id: Option<String>,
    pub(super) caused_by_parent_tool_call_doc_id: Option<String>,
}

pub(super) async fn load_agent_request_for_queue(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<AgentRequest>> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{
                _docID
                request_id
                agent_did
                requester_did
                behavior_id
                session_id
                content
                temperature
                top_p
                top_k
                max_tokens
                metadata
                execution_origin
                created_at
                deadline
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query parent AgentRequest {request_id} for wake-up failed: {:?}",
            response.errors
        );
    }
    let Some(row) = first_row::<AgentRequestQueueRow>(response.data.as_ref(), "AgentRequest")
    else {
        return Ok(None);
    };

    let request = AgentRequest {
        doc_id: row.doc_id,
        request_id: row.request_id,
        agent_did: row.agent_did,
        requester_did: normalize_optional_string(row.requester_did),
        behavior_id: normalize_optional_string(row.behavior_id),
        session_id: row.session_id,
        content: row.content,
        temperature: row.temperature,
        top_p: row.top_p,
        top_k: row.top_k,
        max_tokens: row.max_tokens,
        metadata: row.metadata,
        execution_origin: normalize_optional_string(row.execution_origin),
        created_at: row.created_at,
        deadline: normalize_optional_string(row.deadline),
        subagent_depth: row.subagent_depth.unwrap_or(0),
        caused_by_parent_request_id: normalize_optional_string(row.caused_by_parent_request_id),
        caused_by_parent_request_doc_id: normalize_optional_string(
            row.caused_by_parent_request_doc_id,
        ),
        caused_by_parent_tool_call_id: normalize_optional_string(row.caused_by_parent_tool_call_id),
        caused_by_parent_tool_call_doc_id: normalize_optional_string(
            row.caused_by_parent_tool_call_doc_id,
        ),
    };
    validate_agent_request_subagent_coherence(&request)?;
    Ok(Some(request))
}
