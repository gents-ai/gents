//! R6 background-tool recovery tests.

use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::ToolCallLifecycle;
use gents::{AgentIdentity, DocumentRuntimeOptions, Gents, ToolCeiling};
use serde::Deserialize;

use crate::support::fixtures::{bind_default_behavior_backend, test_identity};
use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::{create_request, first_row, test_db, AGENT_DID};

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    lifecycle_state: Option<String>,
    cancel_cause: Option<String>,
    result: String,
}

#[derive(Debug, Deserialize)]
struct MessageRow {
    content: String,
}

#[derive(Debug, Deserialize)]
struct WakeRequestRow {
    metadata: Option<String>,
}

async fn load_tool_call(node: &EmbeddedNode, tool_call_id: &str) -> ToolCallRow {
    let tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{tool_call_id}" }} }}, limit: 1) {{
                lifecycle_state
                cancel_cause
                result
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
}

async fn load_messages(node: &EmbeddedNode, session_id: &str) -> Vec<MessageRow> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ content }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "load recovery messages failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

async fn load_wakes(node: &EmbeddedNode, session_id: &str) -> Vec<WakeRequestRow> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    execution_origin: {{ _eq: "scheduled" }}
                }}
            ) {{ metadata }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "load recovery wakes failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

async fn create_live_parent_request(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    request_id: &str,
    session_id: &str,
) {
    let agent_did = escape_graphql_string(agent_did);
    let behavior_id = escape_graphql_string(behavior_id);
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "reserved live parent for restart recovery",
                status: "inputRequired",
                lifecycle_state: "inputRequired",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create restart parent request failed: {:?}",
        response.errors
    );
}

async fn boot_agent(db: &crate::support::TestDb, identity: Arc<dyn AgentIdentity>) -> BootedAgent {
    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("load document-backed agent");
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    BootedAgent::new(shutdown_tx, handle, agent_did)
}

async fn wait_for_tool_state(
    node: &EmbeddedNode,
    tool_call_id: &str,
    expected: &str,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let row = load_tool_call(node, tool_call_id).await;
        if row.lifecycle_state.as_deref() == Some(expected) {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for tool_call_id={tool_call_id} to reach {expected}; last={row:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn recover_all_interrupts_backgrounded_running_tool_with_live_parent() {
    let db = test_db("r6-background-recovery-live-parent").await;
    create_request(
        &db.node,
        "r6-recovery-parent",
        "r6-recovery-session",
        "processing",
        "2026-05-14T00:00:00Z",
    )
    .await;

    let mut lifecycle = ToolCallLifecycle::new_background_tool(
        db.node.clone(),
        "r6-recovery-parent".to_string(),
        "r6-recovery-session".to_string(),
        "did:test:test".to_string(),
        "r6-recovery-tool".to_string(),
        1,
        "bash".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
    );
    lifecycle.start_running().await.unwrap();

    let report = ToolCallLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let row = load_tool_call(db.node.as_ref(), "r6-recovery-tool").await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(row.cancel_cause.as_deref(), Some("interrupted"));
    assert!(row.result.contains("interrupted on restart"));

    let messages = load_messages(db.node.as_ref(), "r6-recovery-session").await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.contains(r#"<tool-completion"#));
    assert!(messages[0].content.contains(r#"status="cancelled""#));
    assert!(messages[0]
        .content
        .contains("<reason>interrupted_on_restart</reason>"));

    let wakes = load_wakes(db.node.as_ref(), "r6-recovery-session").await;
    assert_eq!(wakes.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_str(wakes[0].metadata.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["queue"]["source"], "background_completion");
    assert_eq!(
        metadata["queue"]["key"],
        "background_completion:r6-recovery-session"
    );
}

#[tokio::test]
async fn daemon_restart_recovers_background_tool_through_normal_startup_wiring() {
    const REQUEST_ID: &str = "r6-process-boundary-parent";
    const SESSION_ID: &str = "r6-process-boundary-session";
    const TOOL_CALL_ID: &str = "r6-process-boundary-tool";

    let mut db = test_db("r6-background-process-boundary").await;
    let identity: Arc<dyn AgentIdentity> =
        Arc::new(test_identity("r6-background-process-boundary"));
    let agent_did = identity.did().to_string();
    let behavior_id = gents::default_behavior_id_for_agent(&agent_did);
    let endpoint = MockModelEndpoint::start("default").expect("mock inference endpoint");
    bind_default_behavior_backend(
        db.node.as_ref(),
        &agent_did,
        "backend-r6-process-boundary",
        endpoint.endpoint(),
    )
    .await;

    let first_daemon = boot_agent(&db, identity.clone()).await;
    create_live_parent_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        REQUEST_ID,
        SESSION_ID,
    )
    .await;

    let mut lifecycle = ToolCallLifecycle::new_background_tool(
        db.node.clone(),
        REQUEST_ID.to_string(),
        SESSION_ID.to_string(),
        agent_did.clone(),
        TOOL_CALL_ID.to_string(),
        1,
        "bash".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
    );
    lifecycle.start_running().await.unwrap();
    assert_eq!(
        load_tool_call(db.node.as_ref(), TOOL_CALL_ID)
            .await
            .lifecycle_state
            .as_deref(),
        Some("running")
    );

    first_daemon.shutdown().await;
    drop(lifecycle);

    let old_process_generation = db.process_generation;
    db.simulate_process_crash()
        .await
        .expect("reopen the durable store in a new process generation");
    assert_eq!(db.process_generation, old_process_generation + 1);
    assert_eq!(
        load_tool_call(db.node.as_ref(), TOOL_CALL_ID)
            .await
            .lifecycle_state
            .as_deref(),
        Some("running"),
        "the running bridge must survive the durable store reopen"
    );

    let second_daemon = boot_agent(&db, identity).await;
    let row = wait_for_tool_state(db.node.as_ref(), TOOL_CALL_ID, "cancelled").await;
    assert_eq!(row.cancel_cause.as_deref(), Some("interrupted"));
    assert!(row.result.contains("interrupted on restart"));

    let messages = load_messages(db.node.as_ref(), SESSION_ID).await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.contains(r#"<tool-completion"#));
    assert!(messages[0].content.contains(r#"status="cancelled""#));
    assert!(messages[0]
        .content
        .contains("<reason>interrupted_on_restart</reason>"));

    let wakes = load_wakes(db.node.as_ref(), SESSION_ID).await;
    assert_eq!(wakes.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_str(wakes[0].metadata.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["queue"]["source"], "background_completion");
    assert_eq!(
        metadata["queue"]["key"],
        "background_completion:r6-process-boundary-session"
    );

    second_daemon.shutdown().await;
}
