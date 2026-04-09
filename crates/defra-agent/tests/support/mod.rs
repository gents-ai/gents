#![allow(dead_code)]

use std::sync::Arc;

use defra_agent::defra_node::{EmbeddedNode, QueryResponse};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{ensure_runtime_schemas, watcher::AgentRequest};
use serde::Deserialize;
use tempfile::TempDir;

pub mod snapshots;

pub const AGENT_DID: &str = "did:defra-agent:test";
pub const AGENT_NAME: &str = "test";
pub const BACKEND_ID: &str = "backend-test";
pub const DEADLINE_SECS: u64 = 300;

pub struct TestDb {
    pub node: Arc<EmbeddedNode>,
    _tempdir: TempDir,
}

pub async fn test_db(name: &str) -> TestDb {
    let tempdir = tempfile::Builder::new()
        .prefix(&format!("defra-agent-{name}-"))
        .tempdir()
        .expect("tempdir");
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(tempdir.path())
            .build()
            .await
            .expect("embedded node"),
    );
    ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas");
    TestDb {
        node,
        _tempdir: tempdir,
    }
}

pub async fn create_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    status: &str,
    created_at: &str,
) -> String {
    let (lifecycle_state, admission_state) = match status {
        "pending" => ("pending", "released"),
        "processing" => ("processing", "executing"),
        "completed" => ("completed", "released"),
        "error" => ("failed", "released"),
        "superseded" => ("superseded", "released"),
        other => panic!("unsupported test request status: {other}"),
    };
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let created_at = escape_graphql_string(created_at);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "hello",
                status: "{status}",
                lifecycle_state: "{lifecycle_state}",
                admission_state: "{admission_state}",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create request failed: {:?}",
        resp.errors
    );

    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{
                _docID
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_row::<DocIdRow>(&resp, "AgentRequest").doc_id
}

pub async fn create_response_with_status(
    node: &EmbeddedNode,
    response_key: &str,
    request_id: &str,
    session_id: &str,
    status: &str,
) -> String {
    create_response_with_content_and_status(node, response_key, request_id, session_id, "", status)
        .await
}

pub async fn create_response_with_content_and_status(
    node: &EmbeddedNode,
    response_key: &str,
    request_id: &str,
    session_id: &str,
    content: &str,
    status: &str,
) -> String {
    let response_key = escape_graphql_string(response_key);
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let content = escape_graphql_string(content);
    let completed_at = if matches!(status, "complete" | "error") {
        "2026-03-23T00:01:00Z"
    } else {
        ""
    };
    let mutation = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{response_key}",
                request_id: "{request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{session_id}",
                content: "{content}",
                status: "{status}",
                token_count: 0,
                progress_seq: 0,
                created_at: "2026-03-23T00:00:00Z",
                completed_at: "{completed_at}"
            }}) {{ _docID }}
        }}"#,
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create response failed: {:?}",
        resp.errors
    );

    let query = format!(
        r#"{{
            AgentResponse(filter: {{ response_key: {{ _eq: "{response_key}" }} }}) {{
                _docID
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_row::<DocIdRow>(&resp, "AgentResponse").doc_id
}

pub async fn create_response(node: &EmbeddedNode, response_key: &str) -> String {
    create_response_with_status(node, response_key, "req-1", "session-1", "streaming").await
}

pub async fn upsert_conversation(
    node: &EmbeddedNode,
    session_id: &str,
    request_id: &str,
    content: &str,
    status: &str,
) {
    let session_id = escape_graphql_string(session_id);
    let request_id = escape_graphql_string(request_id);
    let content = escape_graphql_string(content);
    let status = escape_graphql_string(status);
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            upsert_AgentConversation(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                add: {{
                    session_id: "{session_id}",
                    agent_name: "{AGENT_NAME}",
                    agent_did: "{AGENT_DID}",
                    behavior_id: "{AGENT_NAME}",
                    title: "Test Conversation",
                    preview_text: "{content}",
                    status: "{status}",
                    created_at: "{now}",
                    updated_at: "{now}",
                    latest_request_id: "{request_id}"
                }},
                update: {{
                    agent_name: "{AGENT_NAME}",
                    agent_did: "{AGENT_DID}",
                    behavior_id: "{AGENT_NAME}",
                    title: "Test Conversation",
                    preview_text: "{content}",
                    status: "{status}",
                    created_at: "{now}",
                    updated_at: "{now}",
                    latest_request_id: "{request_id}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "upsert conversation failed: {:?}",
        resp.errors
    );
}

pub fn build_request(
    doc_id: String,
    request_id: String,
    session_id: String,
    created_at: String,
) -> AgentRequest {
    AgentRequest {
        doc_id,
        request_id,
        agent_did: AGENT_DID.into(),
        behavior_id: Some(AGENT_NAME.into()),
        session_id,
        content: "hello".into(),
        created_at,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DocIdRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
}

pub fn first_row<T>(resp: &QueryResponse, key: &str) -> T
where
    T: for<'de> Deserialize<'de>,
{
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    let value = resp
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .unwrap_or_else(|| panic!("missing row for {key}"));
    serde_json::from_value(value).unwrap_or_else(|err| panic!("decode {key} failed: {err}"))
}

pub fn first_optional_row<T>(resp: &QueryResponse, key: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    resp.data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .map(|value| {
            serde_json::from_value(value).unwrap_or_else(|err| panic!("decode {key} failed: {err}"))
        })
}
