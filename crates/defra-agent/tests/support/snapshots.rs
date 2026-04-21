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
    failure_reason: Option<String>,
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
    pub failure_reason: String,
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
            failure_reason: row.failure_reason.unwrap_or_default(),
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
    #[serde(default)]
    pub forked_from_session_id: Option<String>,
    #[serde(default)]
    pub fork_at_user_turn: Option<i64>,
    #[serde(default)]
    pub forked_at: Option<String>,
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
                failure_reason
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
                forked_from_session_id
                fork_at_user_turn
                forked_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<ConversationSnapshot>(&resp, "AgentConversation")
}

/// Full-column snapshot of an `AgentConversation` row. Use in fork-invariant tests
/// that claim the parent is byte-identical before and after a fork — the narrower
/// `ConversationSnapshot` omits `title`, `preview_text`, `agent_did`, `agent_name`,
/// `created_at`, and `updated_at`, which would hide silent mutations on those fields.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FullConversationSnapshot {
    pub session_id: String,
    pub agent_name: String,
    pub agent_did: String,
    pub behavior_id: String,
    pub title: String,
    pub preview_text: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub latest_request_id: String,
    #[serde(default)]
    pub forked_from_session_id: Option<String>,
    #[serde(default)]
    pub fork_at_user_turn: Option<i64>,
    #[serde(default)]
    pub forked_at: Option<String>,
}

pub async fn fetch_full_conversation_snapshot(
    node: &EmbeddedNode,
    session_id: &str,
) -> Option<FullConversationSnapshot> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                limit: 1
            ) {{
                session_id
                agent_name
                agent_did
                behavior_id
                title
                preview_text
                status
                created_at
                updated_at
                latest_request_id
                forked_from_session_id
                fork_at_user_turn
                forked_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<FullConversationSnapshot>(&resp, "AgentConversation")
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

pub async fn fetch_response_interrupted_at(node: &EmbeddedNode, doc_id: &str) -> Option<String> {
    let doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                limit: 1
            ) {{
                interrupted_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    resp.data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("interrupted_at"))
        .and_then(|value| value.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub async fn fetch_response_content(node: &EmbeddedNode, doc_id: &str) -> String {
    let doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                limit: 1
            ) {{
                content
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    resp.data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("content"))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .unwrap_or_default()
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

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct MessageSnapshot {
    pub message_key: String,
    pub session_id: String,
    pub sequence: u32,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

pub async fn fetch_message_snapshots_for_session(
    node: &EmbeddedNode,
    session_id: &str,
) -> Vec<MessageSnapshot> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                message_key
                session_id
                sequence
                role
                content
                timestamp
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "fetch_message_snapshots failed: {:?}",
        resp.errors
    );
    let data = resp.data.expect("data");
    serde_json::from_value(data["AgentMessage"].clone()).expect("parse MessageSnapshot")
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct ToolCallSnapshot {
    pub tool_call_key: String,
    pub session_id: String,
    pub message_sequence: u32,
    pub tool_name: String,
    pub tool_call_id: String,
    pub args: String,
    pub result: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: String,
}

pub async fn fetch_tool_call_snapshots_for_session(
    node: &EmbeddedNode,
    session_id: &str,
) -> Vec<ToolCallSnapshot> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ message_sequence: ASC }}
            ) {{
                tool_call_key session_id message_sequence tool_name tool_call_id
                args result status started_at completed_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "fetch_tool_call_snapshots failed: {:?}",
        resp.errors
    );
    let data = resp.data.expect("data");
    serde_json::from_value(data["AgentToolCall"].clone()).expect("parse ToolCallSnapshot")
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct ToolResultSnapshot {
    pub agent_did: String,
    pub session_id: String,
    pub tool_name: String,
    pub tool_input: String,
    pub output_text: String,
    pub truncated: bool,
    pub truncation_metadata: String,
    pub conversation_doc_id: String,
    pub created_at: String,
}

pub async fn fetch_tool_result_snapshots_for_session(
    node: &EmbeddedNode,
    session_id: &str,
) -> Vec<ToolResultSnapshot> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentToolResult(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                agent_did session_id tool_name tool_input output_text
                truncated truncation_metadata conversation_doc_id created_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "fetch_tool_result_snapshots failed: {:?}",
        resp.errors
    );
    let data = resp.data.expect("data");
    serde_json::from_value(data["AgentToolResult"].clone()).expect("parse ToolResultSnapshot")
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct CompactionEntrySnapshot {
    pub compaction_key: String,
    pub session_id: String,
    pub sequence: u32,
    pub summary: String,
    pub messages_compacted: u32,
    pub created_at: String,
}

pub async fn fetch_compaction_entry_snapshots_for_session(
    node: &EmbeddedNode,
    session_id: &str,
) -> Vec<CompactionEntrySnapshot> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            CompactionEntry(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                compaction_key session_id sequence summary messages_compacted created_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "fetch_compaction_entry_snapshots failed: {:?}",
        resp.errors
    );
    let data = resp.data.expect("data");
    serde_json::from_value(data["CompactionEntry"].clone()).expect("parse CompactionEntrySnapshot")
}
