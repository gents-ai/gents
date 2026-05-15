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
use defra_agent::{
    default_behavior_id_for_agent, load_agent_behavior, upsert_agent_behavior,
    upsert_tool_selection, AgentBehaviorDocument, AgentIdentity, DefraAgent,
    DocumentRuntimeOptions, ToolCeiling, ToolSelectionDocument,
};
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
    boot_agent_with_policy(db, test_name, None, true, true).await
}

async fn boot_agent_with_targets(
    db: &support::TestDb,
    test_name: &str,
    subagent_targets: Vec<String>,
) -> RunningAgent {
    boot_agent_with_policy(db, test_name, Some(subagent_targets), true, true).await
}

async fn boot_agent_with_policy(
    db: &support::TestDb,
    test_name: &str,
    subagent_targets: Option<Vec<String>>,
    spawn_enabled: bool,
    background_enabled: bool,
) -> RunningAgent {
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity(test_name));
    let agent_did = identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&agent_did);
    let endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        &agent_did,
        "backend-subagent-source",
        endpoint.endpoint(),
    )
    .await;
    ensure_parent_subagent_authorization(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        subagent_targets.unwrap_or_else(|| vec![behavior_id.clone()]),
        spawn_enabled,
        background_enabled,
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

async fn ensure_parent_subagent_authorization(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    subagent_targets: Vec<String>,
    spawn_enabled: bool,
    background_enabled: bool,
) {
    let selection_id = format!("{behavior_id}-r3-subagent-tools");
    upsert_tool_selection(
        node,
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: agent_did.to_string(),
            subagent_targets: Some(subagent_targets),
            subagent_spawn_enabled: Some(spawn_enabled),
            subagent_background_enabled: Some(background_enabled),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let mut behavior = match load_agent_behavior(node, behavior_id).await.unwrap() {
        Some(behavior) => behavior,
        None => AgentBehaviorDocument {
            behavior_id: behavior_id.to_string(),
            agent_did: agent_did.to_string(),
            display_name: Some(behavior_id.to_string()),
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            created_at: Some("2026-05-12T00:00:00Z".to_string()),
        },
    };
    behavior.tool_selection_id = Some(selection_id);
    upsert_agent_behavior(node, &behavior).await.unwrap();
}

#[derive(Debug, Deserialize)]
struct ChildRequestRow {
    request_id: String,
    behavior_id: String,
    content: String,
    lifecycle_state: Option<String>,
    subagent_depth: Option<i64>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_trigger_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    lifecycle_state: Option<String>,
    result: Option<String>,
    tool_failure_class: Option<String>,
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
                lifecycle_state
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

async fn fetch_tool_call(node: &EmbeddedNode, session_id: &str, tool_call_id: &str) -> ToolCallRow {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    tool_call_id: {{ _eq: "{escaped_tool_call_id}" }}
                }},
                limit: 1
            ) {{
                lifecycle_state
                result
                tool_failure_class
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
}

fn assert_tool_call_not_allowed(tool: &ToolCallRow, expected_path: &str, expected_requested: &str) {
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable")
    );
    let result: serde_json::Value =
        serde_json::from_str(tool.result.as_deref().expect("tool result JSON")).unwrap();
    assert_eq!(result["failure_class"], "tool_not_allowed");
    assert_eq!(result["service_id"], "subagent");
    assert_eq!(result["path"], expected_path);
    assert_eq!(result["requested_tool_name"], expected_requested);
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
async fn subagent_source_rejects_unauthorized_target_without_child_request() {
    let db = test_db("r3-subagent-source-unauthorized").await;
    let running = boot_agent_with_targets(&db, "r3-subagent-source-unauthorized", Vec::new()).await;
    let parent_request_id = "r3-parent-unauthorized";
    let parent_tool_call_id = "r3-tc-unauthorized";
    let child_request_id = "r3-child-unauthorized";
    let parent_session_id = "r3-session-unauthorized";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        parent_session_id,
        "parent prompt",
    )
    .await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "unauthorized child prompt from source"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        parent_session_id.to_string(),
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

    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(750),
    )
    .await;
    let tool = fetch_tool_call(db.node.as_ref(), parent_session_id, parent_tool_call_id).await;
    assert_tool_call_not_allowed(&tool, "/behavior_id", &running.behavior_id);

    running.booted.shutdown().await;
}

#[tokio::test]
async fn subagent_source_fails_unauthorized_target_even_when_target_is_not_active() {
    let db = test_db("r3-subagent-source-unauthorized-inactive").await;
    let running =
        boot_agent_with_targets(&db, "r3-subagent-source-unauthorized-inactive", Vec::new()).await;
    let parent_request_id = "r3-parent-unauthorized-inactive";
    let parent_tool_call_id = "r3-tc-unauthorized-inactive";
    let child_request_id = "r3-child-unauthorized-inactive";
    let parent_session_id = "r3-session-unauthorized-inactive";
    let target_behavior_id = "not-active-or-authorized";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        parent_session_id,
        "parent prompt",
    )
    .await;

    let args = serde_json::json!({
        "behavior_id": target_behavior_id,
        "prompt": "unauthorized inactive child prompt from source"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        parent_session_id.to_string(),
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

    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(750),
    )
    .await;
    let tool = fetch_tool_call(db.node.as_ref(), parent_session_id, parent_tool_call_id).await;
    assert_tool_call_not_allowed(&tool, "/behavior_id", target_behavior_id);

    running.booted.shutdown().await;
}

#[tokio::test]
async fn subagent_source_rejects_when_spawn_disabled_even_with_authorized_target() {
    let db = test_db("r3-subagent-source-spawn-disabled").await;
    let running = boot_agent(&db, "r3-subagent-source-spawn-disabled").await;
    ensure_parent_subagent_authorization(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        vec![running.behavior_id.clone()],
        false,
        true,
    )
    .await;
    let parent_request_id = "r3-parent-spawn-disabled";
    let parent_tool_call_id = "r3-tc-spawn-disabled";
    let child_request_id = "r3-child-spawn-disabled";
    let parent_session_id = "r3-session-spawn-disabled";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        parent_session_id,
        "parent prompt",
    )
    .await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "spawn disabled child prompt from source"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        parent_session_id.to_string(),
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

    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(750),
    )
    .await;
    let tool = fetch_tool_call(db.node.as_ref(), parent_session_id, parent_tool_call_id).await;
    assert_tool_call_not_allowed(&tool, "/", "spawn_subagent");

    running.booted.shutdown().await;
}

#[tokio::test]
async fn subagent_source_rejects_background_when_background_disabled() {
    let db = test_db("r3-subagent-source-background-disabled").await;
    let running = boot_agent(&db, "r3-subagent-source-background-disabled").await;
    ensure_parent_subagent_authorization(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        vec![running.behavior_id.clone()],
        true,
        false,
    )
    .await;
    let parent_request_id = "r3-parent-background-disabled";
    let parent_tool_call_id = "r3-tc-background-disabled";
    let child_request_id = "r3-child-background-disabled";
    let parent_session_id = "r3-session-background-disabled";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        parent_session_id,
        "parent prompt",
    )
    .await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "background disabled child prompt from source",
        "await_mode": "background"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        parent_session_id.to_string(),
        parent_tool_call_id.to_string(),
        1,
        "spawn_subagent".to_string(),
        args,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Background,
        CancelPolicy::Cascade,
        child_request_id.to_string(),
    );
    lifecycle.start_running().await.unwrap();

    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(750),
    )
    .await;
    let tool = fetch_tool_call(db.node.as_ref(), parent_session_id, parent_tool_call_id).await;
    assert_tool_call_not_allowed(&tool, "/await_mode", "background");

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

#[tokio::test]
async fn recovery_materializes_orphan_child_request_for_running_subagent_tool() {
    let db = test_db("r3-subagent-source-orphan-recovery").await;
    let parent_request_id = "r3-parent-orphan";
    let parent_session_id = "r3-session-orphan";
    let parent_tool_call_id = "r3-tc-orphan";
    let child_request_id = "child-orphan-1";
    ensure_parent_subagent_authorization(
        db.node.as_ref(),
        support::AGENT_DID,
        support::AGENT_NAME,
        vec![support::AGENT_NAME.to_string()],
        true,
        true,
    )
    .await;
    support::create_request(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        "processing",
        &chrono::Utc::now().to_rfc3339(),
    )
    .await;
    create_orphan_subagent_tool_call(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
    )
    .await;

    let report = ToolCallLifecycle::recover_all(db.node.as_ref(), support::AGENT_DID)
        .await
        .unwrap();
    assert_eq!(
        report.tool_calls_recovered, 0,
        "orphan backfill should not terminalize a live non-expired tool call"
    );

    let child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_eq!(child.request_id, child_request_id);
    assert_eq!(child.behavior_id, support::AGENT_NAME);
    assert_eq!(child.content, "orphan child prompt");
    assert_eq!(child.lifecycle_state.as_deref(), Some("pending"));
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
}

#[tokio::test]
async fn recovery_rejects_unauthorized_orphan_child_request() {
    let db = test_db("r3-subagent-source-orphan-unauthorized").await;
    let parent_request_id = "r3-parent-orphan-denied";
    let parent_session_id = "r3-session-orphan-denied";
    let parent_tool_call_id = "r3-tc-orphan-denied";
    let child_request_id = "child-orphan-denied";
    ensure_parent_subagent_authorization(
        db.node.as_ref(),
        support::AGENT_DID,
        support::AGENT_NAME,
        Vec::new(),
        true,
        true,
    )
    .await;
    support::create_request(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        "processing",
        &chrono::Utc::now().to_rfc3339(),
    )
    .await;
    create_orphan_subagent_tool_call(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
    )
    .await;

    let report = ToolCallLifecycle::recover_all(db.node.as_ref(), support::AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 0);
    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(0),
    )
    .await;

    let tool = fetch_tool_call(db.node.as_ref(), parent_session_id, parent_tool_call_id).await;
    assert_tool_call_not_allowed(&tool, "/behavior_id", support::AGENT_NAME);
}

#[tokio::test]
async fn recovery_rejects_orphan_when_spawn_disabled_even_with_authorized_target() {
    let db = test_db("r3-subagent-source-orphan-spawn-disabled").await;
    let parent_request_id = "r3-parent-orphan-spawn-disabled";
    let parent_session_id = "r3-session-orphan-spawn-disabled";
    let parent_tool_call_id = "r3-tc-orphan-spawn-disabled";
    let child_request_id = "child-orphan-spawn-disabled";
    ensure_parent_subagent_authorization(
        db.node.as_ref(),
        support::AGENT_DID,
        support::AGENT_NAME,
        vec![support::AGENT_NAME.to_string()],
        false,
        true,
    )
    .await;
    support::create_request(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        "processing",
        &chrono::Utc::now().to_rfc3339(),
    )
    .await;
    create_orphan_subagent_tool_call(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
    )
    .await;

    let report = ToolCallLifecycle::recover_all(db.node.as_ref(), support::AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 0);
    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(0),
    )
    .await;
    let tool = fetch_tool_call(db.node.as_ref(), parent_session_id, parent_tool_call_id).await;
    assert_tool_call_not_allowed(&tool, "/", "spawn_subagent");
}

#[tokio::test]
async fn recovery_rejects_background_orphan_when_background_disabled() {
    let db = test_db("r3-subagent-source-orphan-background-disabled").await;
    let parent_request_id = "r3-parent-orphan-background-disabled";
    let parent_session_id = "r3-session-orphan-background-disabled";
    let parent_tool_call_id = "r3-tc-orphan-background-disabled";
    let child_request_id = "child-orphan-background-disabled";
    ensure_parent_subagent_authorization(
        db.node.as_ref(),
        support::AGENT_DID,
        support::AGENT_NAME,
        vec![support::AGENT_NAME.to_string()],
        true,
        false,
    )
    .await;
    support::create_request(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        "processing",
        &chrono::Utc::now().to_rfc3339(),
    )
    .await;
    create_orphan_subagent_tool_call_with_await_mode(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
        AwaitMode::Background,
    )
    .await;

    let report = ToolCallLifecycle::recover_all(db.node.as_ref(), support::AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 0);
    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(0),
    )
    .await;
    let tool = fetch_tool_call(db.node.as_ref(), parent_session_id, parent_tool_call_id).await;
    assert_tool_call_not_allowed(&tool, "/await_mode", "background");
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

async fn create_orphan_subagent_tool_call(
    node: &EmbeddedNode,
    parent_request_id: &str,
    parent_session_id: &str,
    parent_tool_call_id: &str,
    child_request_id: &str,
) {
    create_orphan_subagent_tool_call_with_await_mode(
        node,
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
        AwaitMode::Foreground,
    )
    .await;
}

async fn create_orphan_subagent_tool_call_with_await_mode(
    node: &EmbeddedNode,
    parent_request_id: &str,
    parent_session_id: &str,
    parent_tool_call_id: &str,
    child_request_id: &str,
    await_mode: AwaitMode,
) {
    let escaped_parent_request_id = escape_graphql_string(parent_request_id);
    let escaped_parent_session_id = escape_graphql_string(parent_session_id);
    let escaped_parent_tool_call_id = escape_graphql_string(parent_tool_call_id);
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let tool_call_key = format!("{escaped_parent_session_id}:{escaped_parent_tool_call_id}");
    let args = serde_json::json!({
        "behavior_id": support::AGENT_NAME,
        "prompt": "orphan child prompt"
    })
    .to_string();
    let escaped_args = escape_graphql_string(&args);
    let started_at = chrono::Utc::now().to_rfc3339();
    let deadline_at = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    let await_mode = await_mode.as_str();
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{escaped_parent_request_id}",
                session_id: "{escaped_parent_session_id}",
                message_sequence: 1,
                tool_name: "spawn_subagent",
                tool_call_id: "{escaped_parent_tool_call_id}",
                args: "{escaped_args}",
                result: "",
                status: "called",
                lifecycle_state: "running",
                started_at: "{started_at}",
                deadline_at: "{deadline_at}",
                child_request_id: "{escaped_child_request_id}",
                await_mode: "{await_mode}",
                cancel_policy: "cascade",
                selected_service_id: null,
                selected_tool_name: null,
                tool_failure_class: null,
                latency_ms: null
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create orphan AgentToolCall failed: {:?}",
        response.errors
    );
}
