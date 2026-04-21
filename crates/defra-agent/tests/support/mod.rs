#![allow(dead_code)]

use std::sync::Arc;

use defra_agent::defra_node::{EmbeddedNode, QueryResponse};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{ensure_runtime_schemas, watcher::AgentRequest};
use serde::Deserialize;
use tempfile::TempDir;

pub mod fixtures;
pub mod http_mock;
pub mod mock_endpoint;
pub mod snapshots;
pub mod waits;

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
    let lifecycle_state = match status {
        "pending" => "pending",
        "processing" => "processing",
        "completed" => "completed",
        "error" => "failed",
        "superseded" => "superseded",
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

pub async fn set_interrupt_requested_at(node: &EmbeddedNode, doc_id: &str, at: &str) {
    let doc_id = escape_graphql_string(doc_id);
    let at = escape_graphql_string(at);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ interrupt_requested_at: "{at}" }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "set_interrupt_requested_at failed: {:?}",
        resp.errors
    );
}

pub async fn set_request_lifecycle_state(node: &EmbeddedNode, doc_id: &str, lifecycle_state: &str) {
    let doc_id = escape_graphql_string(doc_id);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ lifecycle_state: "{lifecycle_state}" }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "set_request_lifecycle_state failed: {:?}",
        resp.errors
    );
}

pub async fn set_valid_until(node: &EmbeddedNode, doc_id: &str, at: &str) {
    let doc_id = escape_graphql_string(doc_id);
    let at = escape_graphql_string(at);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ valid_until: "{at}" }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "set_valid_until failed: {:?}",
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
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        created_at,
    }
}

pub async fn create_agent_session(
    node: &EmbeddedNode,
    session_id: &str,
    behavior_id: &str,
    started: &str,
) {
    let session_id = escape_graphql_string(session_id);
    let behavior_id = escape_graphql_string(behavior_id);
    let started = escape_graphql_string(started);
    let mutation = format!(
        r#"mutation {{
            create_AgentSession(input: {{
                session_id: "{session_id}",
                agent_name: "{AGENT_NAME}",
                behavior_id: "{behavior_id}",
                started: "{started}",
                status: "active"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_AgentSession failed: {:?}",
        resp.errors
    );
}

pub async fn create_agent_conversation(
    node: &EmbeddedNode,
    session_id: &str,
    behavior_id: &str,
    created_at: &str,
) {
    let session_id_escaped = escape_graphql_string(session_id);
    let behavior_id_escaped = escape_graphql_string(behavior_id);
    let created_at_escaped = escape_graphql_string(created_at);
    let mutation = format!(
        r#"mutation {{
            create_AgentConversation(input: {{
                session_id: "{session_id_escaped}",
                agent_name: "{AGENT_NAME}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{behavior_id_escaped}",
                title: "test conversation",
                preview_text: "",
                status: "active",
                created_at: "{created_at_escaped}",
                updated_at: "{created_at_escaped}",
                latest_request_id: ""
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_AgentConversation failed: {:?}",
        resp.errors
    );
}

pub async fn create_agent_message(
    node: &EmbeddedNode,
    session_id: &str,
    sequence: u32,
    role: &str,
    content: &str,
    timestamp: &str,
) {
    let session_id_escaped = escape_graphql_string(session_id);
    let role_escaped = escape_graphql_string(role);
    let content_escaped = escape_graphql_string(content);
    let timestamp_escaped = escape_graphql_string(timestamp);
    let message_key = format!("{session_id_escaped}:{sequence}");
    let mutation = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{session_id_escaped}",
                sequence: {sequence},
                role: "{role_escaped}",
                content: "{content_escaped}",
                timestamp: "{timestamp_escaped}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_AgentMessage failed: {:?}",
        resp.errors
    );
}

pub async fn create_agent_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    message_sequence: u32,
    tool_call_id: &str,
    tool_name: &str,
    args: &str,
    result: &str,
    status: &str,
    started_at: &str,
    completed_at: &str,
) {
    let session_id_escaped = escape_graphql_string(session_id);
    let tool_call_id_escaped = escape_graphql_string(tool_call_id);
    let tool_name_escaped = escape_graphql_string(tool_name);
    let args_escaped = escape_graphql_string(args);
    let result_escaped = escape_graphql_string(result);
    let status_escaped = escape_graphql_string(status);
    let started_escaped = escape_graphql_string(started_at);
    let completed_escaped = escape_graphql_string(completed_at);
    let tool_call_key = format!("{session_id_escaped}:{tool_call_id_escaped}");
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                session_id: "{session_id_escaped}",
                message_sequence: {message_sequence},
                tool_name: "{tool_name_escaped}",
                tool_call_id: "{tool_call_id_escaped}",
                args: "{args_escaped}",
                result: "{result_escaped}",
                status: "{status_escaped}",
                started_at: "{started_escaped}",
                completed_at: "{completed_escaped}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_AgentToolCall failed: {:?}",
        resp.errors
    );
}

pub async fn create_agent_tool_result(
    node: &EmbeddedNode,
    session_id: &str,
    tool_name: &str,
    tool_input: &str,
    output_text: &str,
    created_at: &str,
) {
    let session_id_escaped = escape_graphql_string(session_id);
    let tool_name_escaped = escape_graphql_string(tool_name);
    let tool_input_escaped = escape_graphql_string(tool_input);
    let output_text_escaped = escape_graphql_string(output_text);
    let created_at_escaped = escape_graphql_string(created_at);
    let mutation = format!(
        r#"mutation {{
            create_AgentToolResult(input: {{
                agent_did: "{AGENT_DID}",
                session_id: "{session_id_escaped}",
                tool_name: "{tool_name_escaped}",
                tool_input: "{tool_input_escaped}",
                output_text: "{output_text_escaped}",
                truncated: false,
                truncation_metadata: "",
                conversation_doc_id: "",
                created_at: "{created_at_escaped}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_AgentToolResult failed: {:?}",
        resp.errors
    );
}

pub async fn create_compaction_entry(
    node: &EmbeddedNode,
    session_id: &str,
    sequence: u32,
    summary: &str,
    messages_compacted: u32,
    created_at: &str,
) {
    let session_id_escaped = escape_graphql_string(session_id);
    let summary_escaped = escape_graphql_string(summary);
    let created_at_escaped = escape_graphql_string(created_at);
    let compaction_key = format!("{session_id_escaped}:{sequence}");
    let mutation = format!(
        r#"mutation {{
            create_CompactionEntry(input: {{
                compaction_key: "{compaction_key}",
                session_id: "{session_id_escaped}",
                sequence: {sequence},
                summary: "{summary_escaped}",
                files_read: "[]",
                files_modified: "[]",
                messages_compacted: {messages_compacted},
                original_tokens: 100,
                compacted_tokens: 50,
                created_at: "{created_at_escaped}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_CompactionEntry failed: {:?}",
        resp.errors
    );
}

pub async fn create_agent_behavior(node: &EmbeddedNode, behavior_id: &str, agent_did: &str) {
    let behavior_id_escaped = escape_graphql_string(behavior_id);
    let agent_did_escaped = escape_graphql_string(agent_did);
    let mutation = format!(
        r#"mutation {{
            create_AgentBehavior(input: {{
                behavior_id: "{behavior_id_escaped}",
                agent_did: "{agent_did_escaped}",
                display_name: "test behavior",
                system_prompt: "",
                backend_id: "{BACKEND_ID}",
                model_name: "test-model",
                tool_selection_id: "",
                inference_profile_id: "",
                compaction_strategy: "StripThenSummarize",
                compaction_threshold: 0.75,
                enabled: true,
                created_at: "2026-04-21T00:00:00Z"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_AgentBehavior failed: {:?}",
        resp.errors
    );
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
