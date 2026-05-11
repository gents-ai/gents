//! Bucket 3 runtime conformance for R3 SubagentSource.

mod support;

use std::sync::Arc;
use std::time::Duration;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::interrupt::{fetch_interrupt_requested_at, interrupt_request};
use defra_agent::tool_call_lifecycle::{
    create_subagent_request_with_request_id, AwaitMode, CancelPolicy, IllegalToolCallTransition,
    ToolCallLifecycle, MAX_SUBAGENT_DEPTH,
};
use defra_agent::{AgentIdentity, DefraAgent, DocumentRuntimeOptions, ToolCeiling};
use serde::Deserialize;

use support::fixtures::{bind_default_behavior_backend, test_identity};
use support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use support::mock_endpoint::MockModelEndpoint;
use support::{first_optional_row, first_row, test_db};

struct RunningAgent {
    booted: BootedAgent,
    _endpoint: MockModelEndpoint,
    behavior_id: String,
}

async fn boot_agent(db: &support::TestDb, test_name: &str) -> RunningAgent {
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity(test_name));
    let endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        "backend-subagent-source",
        endpoint.endpoint(),
    )
    .await;
    let agent = DefraAgent::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_did = agent.agent_did().to_string();
    let behavior_id = agent.default_behavior_id().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    RunningAgent {
        booted: BootedAgent::new(shutdown_tx, handle, agent_did),
        _endpoint: endpoint,
        behavior_id,
    }
}

#[derive(Debug, Deserialize)]
struct ChildRequestRow {
    request_id: String,
    behavior_id: String,
    content: String,
    subagent_depth: Option<i64>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_trigger_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
}

async fn wait_for_child_request(node: &EmbeddedNode, child_request_id: &str) -> ChildRequestRow {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{
                request_id
                behavior_id
                content
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let response = node.execute(&query).await;
        if let Some(row) = first_optional_row::<ChildRequestRow>(&response, "AgentRequest") {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for child AgentRequest {child_request_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn assert_no_child_request_for_tool(
    node: &EmbeddedNode,
    parent_tool_call_id: &str,
    settle: Duration,
) {
    tokio::time::sleep(settle).await;
    let escaped_parent_tool_call_id = escape_graphql_string(parent_tool_call_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    caused_by_parent_tool_call_id: {{ _eq: "{escaped_parent_tool_call_id}" }}
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query failed: {:?}",
        response.errors
    );
    let count = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    assert_eq!(
        count, 0,
        "native AgentToolCall unexpectedly spawned {count} child request(s)"
    );
}

#[tokio::test]
async fn subagent_source_materializes_child_request_from_tool_call() {
    let db = test_db("r3-subagent-source-spawn").await;
    let running = boot_agent(&db, "r3-subagent-source-spawn").await;
    let parent_request_id = "r3-parent-spawn";
    let parent_tool_call_id = "r3-tc-spawn";
    let child_request_id = "r3-child-spawn";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        "r3-session-spawn",
        "parent prompt",
    )
    .await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "child prompt from source"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        "r3-session-spawn".to_string(),
        parent_tool_call_id.to_string(),
        1,
        "spawn_subagent".to_string(),
        args,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        child_request_id.to_string(),
    );
    lifecycle.start_running().await.unwrap();

    let child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_eq!(child.request_id, child_request_id);
    assert_eq!(child.behavior_id, running.behavior_id);
    assert_eq!(child.content, "child prompt from source");
    assert_eq!(child.subagent_depth, Some(1));
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(parent_request_id)
    );
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        Some(parent_tool_call_id)
    );
    assert_eq!(
        child.caused_by_trigger_id.as_deref(),
        Some(parent_tool_call_id)
    );
    assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("subagent"));

    running.booted.shutdown().await;
}

#[tokio::test]
async fn subagent_source_ignores_native_tool_call_without_child_request_id() {
    let db = test_db("r3-subagent-source-native").await;
    let running = boot_agent(&db, "r3-subagent-source-native").await;
    let parent_request_id = "r3-parent-native";
    let parent_tool_call_id = "r3-tc-native";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        "r3-session-native",
        "parent prompt",
    )
    .await;

    let mut lifecycle = ToolCallLifecycle::new(
        db.node.clone(),
        parent_request_id.to_string(),
        "r3-session-native".to_string(),
        parent_tool_call_id.to_string(),
        1,
        "read_file".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
    );
    lifecycle.start_running().await.unwrap();

    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(750),
    )
    .await;

    running.booted.shutdown().await;
}

#[tokio::test]
async fn create_subagent_request_rejects_nonexistent_parent_request() {
    let db = test_db("r3-subagent-source-missing-parent").await;
    let error = create_subagent_request_with_request_id(
        db.node.as_ref(),
        "r3-child-missing-parent".to_string(),
        "missing-parent-request".to_string(),
        "r3-tc-missing-parent".to_string(),
        0,
        support::AGENT_DID.to_string(),
        "test".to_string(),
        "prompt".to_string(),
        None,
    )
    .await
    .expect_err("missing parent request must be rejected");
    assert!(
        matches!(
            error.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::ParentLinkageIncoherent)
        ),
        "expected ParentLinkageIncoherent, got {error:?}"
    );
}

#[tokio::test]
async fn create_subagent_request_enforces_depth_boundary() {
    let db = test_db("r3-subagent-source-depth").await;
    support::create_request(
        db.node.as_ref(),
        "r3-parent-depth",
        "r3-session-depth",
        "processing",
        &chrono::Utc::now().to_rfc3339(),
    )
    .await;

    let child_id = create_subagent_request_with_request_id(
        db.node.as_ref(),
        "r3-child-depth-max".to_string(),
        "r3-parent-depth".to_string(),
        "r3-tc-depth-max".to_string(),
        MAX_SUBAGENT_DEPTH - 1,
        support::AGENT_DID.to_string(),
        "test".to_string(),
        "prompt".to_string(),
        None,
    )
    .await
    .expect("parent depth MAX-1 should create a child at MAX");
    let escaped_child_id = escape_graphql_string(&child_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_child_id}" }} }}, limit: 1) {{
                subagent_depth
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct DepthRow {
        subagent_depth: i64,
    }
    let row = first_row::<DepthRow>(&db.node.execute(&query).await, "AgentRequest");
    assert_eq!(row.subagent_depth, i64::from(MAX_SUBAGENT_DEPTH));

    let error = create_subagent_request_with_request_id(
        db.node.as_ref(),
        "r3-child-depth-over".to_string(),
        "r3-parent-depth".to_string(),
        "r3-tc-depth-over".to_string(),
        MAX_SUBAGENT_DEPTH,
        support::AGENT_DID.to_string(),
        "test".to_string(),
        "prompt".to_string(),
        None,
    )
    .await
    .expect_err("parent depth MAX should be rejected");
    assert!(
        matches!(
            error.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::SubagentDepthExceeded)
        ),
        "expected SubagentDepthExceeded, got {error:?}"
    );
}

#[tokio::test]
async fn cascade_after_source_spawn_reaches_child_request() {
    let db = test_db("r3-subagent-source-cascade").await;
    let running = boot_agent(&db, "r3-subagent-source-cascade").await;
    let parent_request_id = "r3-parent-cascade";
    let parent_tool_call_id = "r3-tc-cascade";
    let child_request_id = "r3-child-cascade";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        "r3-session-cascade",
        "parent prompt",
    )
    .await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "child prompt for cascade"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        "r3-session-cascade".to_string(),
        parent_tool_call_id.to_string(),
        1,
        "spawn_subagent".to_string(),
        args,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        child_request_id.to_string(),
    );
    lifecycle.start_running().await.unwrap();
    let _child = wait_for_child_request(db.node.as_ref(), child_request_id).await;

    interrupt_request(db.node.as_ref(), parent_request_id)
        .await
        .unwrap();
    mark_request_interrupted(db.node.as_ref(), parent_request_id).await;
    ToolCallLifecycle::recover_all(db.node.as_ref(), &running.booted.agent_did)
        .await
        .unwrap();

    let child_interrupt = fetch_interrupt_requested_at(db.node.as_ref(), child_request_id)
        .await
        .unwrap();
    assert!(
        child_interrupt.is_some(),
        "cascade recovery should latch child interrupt_requested_at"
    );

    running.booted.shutdown().await;
}

async fn mark_request_interrupted(node: &EmbeddedNode, request_id: &str) {
    let escaped_request_id = escape_graphql_string(request_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                input: {{
                    status: "error",
                    lifecycle_state: "interrupted"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "mark_request_interrupted failed: {:?}",
        response.errors
    );
}
