use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use serde::Deserialize;

use super::{first_optional_row, first_row};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RequestSnapshotRow {
    status: String,
    lifecycle_state: String,
    behavior_id: String,
    backend_id: String,
    execution_origin: String,
    retry_parent_request: String,
    retry_root_request: String,
    superseded_by_request: String,
    retry_count: i64,
    max_retries: i64,
    claimed_at: Option<String>,
    deadline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSnapshot {
    pub status: String,
    pub lifecycle_state: String,
    pub behavior_id: String,
    pub backend_id: String,
    pub execution_origin: String,
    pub retry_parent_request: String,
    pub retry_root_request: String,
    pub superseded_by_request: String,
    pub retry_count: i64,
    pub max_retries: i64,
    pub claimed_at_present: bool,
    pub deadline_present: bool,
}

impl From<RequestSnapshotRow> for RequestSnapshot {
    fn from(row: RequestSnapshotRow) -> Self {
        Self {
            status: row.status,
            lifecycle_state: row.lifecycle_state,
            behavior_id: row.behavior_id,
            backend_id: row.backend_id,
            execution_origin: row.execution_origin,
            retry_parent_request: row.retry_parent_request,
            retry_root_request: row.retry_root_request,
            superseded_by_request: row.superseded_by_request,
            retry_count: row.retry_count,
            max_retries: row.max_retries,
            claimed_at_present: row
                .claimed_at
                .as_deref()
                .is_some_and(|value| !value.is_empty()),
            deadline_present: row
                .deadline
                .as_deref()
                .is_some_and(|value| !value.is_empty()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RequestLineageSnapshot {
    #[serde(default)]
    pub caused_by_trigger_id: Option<String>,
    #[serde(default)]
    pub caused_by_trigger_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConversationSnapshot {
    pub latest_request_id: String,
    pub behavior_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub behavior_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ResponseSnapshotRow {
    status: String,
    behavior_id: String,
    progress_seq: i64,
    completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseSnapshot {
    pub status: String,
    pub behavior_id: String,
    pub progress_seq: i64,
    pub completed_at_present: bool,
}

impl From<ResponseSnapshotRow> for ResponseSnapshot {
    fn from(row: ResponseSnapshotRow) -> Self {
        Self {
            status: row.status,
            behavior_id: row.behavior_id,
            progress_seq: row.progress_seq,
            completed_at_present: row
                .completed_at
                .as_deref()
                .is_some_and(|value| !value.is_empty()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RuntimeSnapshot {
    pub process_state: String,
    pub reconcile_phase: String,
    pub active_generation: i64,
    pub router_generation: i64,
    pub default_behavior_id: String,
    pub runnable_behavior_count: i64,
    pub unavailable_behavior_count: i64,
    pub last_reconcile_result: String,
    pub last_reconcile_error: String,
}

pub async fn fetch_request_snapshot(node: &EmbeddedNode, doc_id: &str) -> RequestSnapshot {
    let doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                limit: 1
            ) {{
                status
                lifecycle_state
                behavior_id
                backend_id
                execution_origin
                retry_parent_request
                retry_root_request
                superseded_by_request
                retry_count
                max_retries
                claimed_at
                deadline
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_row::<RequestSnapshotRow>(&resp, "AgentRequest").into()
}

pub async fn fetch_request_lineage_snapshot(
    node: &EmbeddedNode,
    doc_id: &str,
) -> RequestLineageSnapshot {
    let doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                limit: 1
            ) {{
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_row::<RequestLineageSnapshot>(&resp, "AgentRequest")
}

pub async fn fetch_request_lineage_snapshot_by_tuple(
    node: &EmbeddedNode,
    trigger_id: &str,
    trigger_kind: &str,
) -> Option<RequestLineageSnapshot> {
    let trigger_id = escape_graphql_string(trigger_id);
    let trigger_kind = escape_graphql_string(trigger_kind);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    _and: [
                        {{ caused_by_trigger_id: {{ _eq: "{trigger_id}" }} }},
                        {{ caused_by_trigger_kind: {{ _eq: "{trigger_kind}" }} }}
                    ]
                }},
                limit: 1
            ) {{
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<RequestLineageSnapshot>(&resp, "AgentRequest")
}

pub async fn fetch_conversation_snapshot(
    node: &EmbeddedNode,
    session_id: &str,
) -> Option<ConversationSnapshot> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                limit: 1
            ) {{
                latest_request_id
                behavior_id
                status
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<ConversationSnapshot>(&resp, "AgentConversation")
}

pub async fn fetch_session_snapshot(
    node: &EmbeddedNode,
    session_id: &str,
) -> Option<SessionSnapshot> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                limit: 1
            ) {{
                session_id
                behavior_id
                status
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<SessionSnapshot>(&resp, "AgentSession")
}

pub async fn fetch_response_snapshot(node: &EmbeddedNode, doc_id: &str) -> ResponseSnapshot {
    let doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                limit: 1
            ) {{
                status
                behavior_id
                progress_seq
                completed_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_row::<ResponseSnapshotRow>(&resp, "AgentResponse").into()
}

pub async fn fetch_runtime_snapshot(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Option<RuntimeSnapshot> {
    let agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                limit: 1
            ) {{
                process_state
                reconcile_phase
                active_generation
                router_generation
                default_behavior_id
                runnable_behavior_count
                unavailable_behavior_count
                last_reconcile_result
                last_reconcile_error
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<RuntimeSnapshot>(&resp, "AgentRuntime")
}
