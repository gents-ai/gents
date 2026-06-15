//! Bucket 3 runtime conformance for R3 SubagentSource.

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

use crate::support::fixtures::{bind_default_behavior_backend, test_identity};
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::{first_optional_row, first_row, test_db};

struct RunningAgent {
    booted: BootedAgent,
    _endpoint: MockModelEndpoint,
    behavior_id: String,
}

async fn boot_agent(db: &crate::support::TestDb, test_name: &str) -> RunningAgent {
    boot_agent_with_policy(db, test_name, None, true, true).await
}

async fn boot_agent_with_targets(
    db: &crate::support::TestDb,
    test_name: &str,
    subagent_targets: Vec<String>,
) -> RunningAgent {
    boot_agent_with_policy(db, test_name, Some(subagent_targets), true, true).await
}

async fn boot_agent_with_policy(
    db: &crate::support::TestDb,
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
    // Each bare behavior id becomes a named local target whose `name` equals the
    // behavior id (so the model-facing spawn args, which carry `behavior_id`
    // / fall back to it as the name, still match an allowed target).
    let target_entries = subagent_targets
        .into_iter()
        .map(|target_behavior_id| {
            defra_agent::subagent_target_entry(
                target_behavior_id.clone(),
                agent_did,
                target_behavior_id,
                None,
            )
        })
        .collect();
    upsert_tool_selection(
        node,
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: agent_did.to_string(),
            subagent_targets: Some(target_entries),
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
            description: None,
            summary: None,
            system_prompt: None,
            system_context_template: None,
            request_context_template: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
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
        "did:defra-agent:test".to_string(),
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
        "did:defra-agent:test".to_string(),
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
    assert_tool_call_not_allowed(&tool, "/name", &running.behavior_id);

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
        "did:defra-agent:test".to_string(),
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
    assert_tool_call_not_allowed(&tool, "/name", target_behavior_id);

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
        "did:defra-agent:test".to_string(),
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
        "did:defra-agent:test".to_string(),
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
        "did:defra-agent:test".to_string(),
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
        crate::support::AGENT_DID.to_string(),
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
    crate::support::create_request(
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
        crate::support::AGENT_DID.to_string(),
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
        crate::support::AGENT_DID.to_string(),
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
        "did:defra-agent:test".to_string(),
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

/// Change 2 (#377): DID-anchored single-creator gate. On the non-trusted path a
/// node must NOT materialize a child whose resolved target DID is not its own
/// local DID. The peer that owns the target DID is the single creator.
#[tokio::test]
async fn subagent_source_skips_child_when_resolved_did_is_remote() {
    let db = test_db("r3-subagent-source-did-anchor").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("r3-did-anchor"));
    let agent_did = identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&agent_did);
    let endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        &agent_did,
        "backend-did-anchor",
        endpoint.endpoint(),
    )
    .await;

    // Authorize a target named "remote-target" owned by a DIFFERENT (remote) DID.
    // Cross-deployment is enabled so the spawn is authorized; the DID-anchor gate
    // in SubagentSource is what must prevent this (non-owning) node from creating
    // the child.
    let remote_did = "did:key:zRemotePeerNotUs";
    let selection_id = format!("{behavior_id}-r3-did-anchor-tools");
    upsert_tool_selection(
        db.node.as_ref(),
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: agent_did.clone(),
            subagent_targets: Some(vec![defra_agent::subagent_target_entry(
                "remote-target",
                remote_did,
                "remote-behavior",
                None,
            )]),
            subagent_spawn_enabled: Some(true),
            subagent_background_enabled: Some(true),
            subagent_allow_cross_deployment: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut behavior = AgentBehaviorDocument {
        behavior_id: behavior_id.clone(),
        agent_did: agent_did.clone(),
        display_name: Some(behavior_id.clone()),
        description: None,
        summary: None,
        system_prompt: None,
        system_context_template: None,
        request_context_template: None,
        backend_id: None,
        model_name: None,
        tool_selection_id: Some(selection_id.clone()),
        inference_profile_id: None,
        compaction_strategy: None,
        compaction_threshold: None,
        skill_refs: Vec::new(),
        skill_excludes: Vec::new(),
        enabled: true,
        created_at: Some("2026-06-04T00:00:00Z".to_string()),
    };
    if let Some(existing) = load_agent_behavior(db.node.as_ref(), &behavior_id)
        .await
        .unwrap()
    {
        behavior = existing;
        behavior.tool_selection_id = Some(selection_id.clone());
    }
    upsert_agent_behavior(db.node.as_ref(), &behavior)
        .await
        .unwrap();

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
    let booted = BootedAgent::new(shutdown_tx, handle, agent_did.clone());

    let parent_request_id = "r3-parent-did-anchor";
    let parent_tool_call_id = "r3-tc-did-anchor";
    let child_request_id = "r3-child-did-anchor";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        parent_request_id,
        "r3-session-did-anchor",
        "parent prompt",
    )
    .await;

    // Bridge args carry the RESOLVED remote target DID, as the spawn hook writes.
    let args = serde_json::json!({
        "name": "remote-target",
        "agent_did": remote_did,
        "behavior_id": "remote-behavior",
        "prompt": "child prompt that should not run here"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        "r3-session-did-anchor".to_string(),
        "did:defra-agent:test".to_string(),
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

    // This node does not own the remote DID, so it must not create the child.
    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(800),
    )
    .await;

    booted.shutdown().await;
}

/// Change 3 (#377): orphan-child-escapes-cancel race. If the parent is
/// interrupted before `SubagentSource` materializes the child, the source must
/// re-check after the create and interrupt the just-created child so it does not
/// run uncancellable.
#[tokio::test]
async fn subagent_source_interrupts_child_when_parent_already_interrupted() {
    let db = test_db("r3-subagent-source-orphan-cancel").await;
    let running = boot_agent(&db, "r3-subagent-source-orphan-cancel").await;
    let parent_request_id = "r3-parent-orphan-cancel";
    let parent_tool_call_id = "r3-tc-orphan-cancel";
    let child_request_id = "r3-child-orphan-cancel";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        "r3-session-orphan-cancel",
        "parent prompt",
    )
    .await;

    // Latch the parent interrupt BEFORE writing the running bridge, so by the time
    // SubagentSource creates the child the parent is already interrupted. The
    // post-create re-check must then interrupt the orphan child.
    interrupt_request(db.node.as_ref(), parent_request_id)
        .await
        .unwrap();

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "child prompt racing a parent cancel"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        "r3-session-orphan-cancel".to_string(),
        "did:defra-agent:test".to_string(),
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

    // The child gets materialized, then immediately interrupted by the source.
    let _child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if fetch_interrupt_requested_at(db.node.as_ref(), child_request_id)
            .await
            .unwrap()
            .is_some()
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "child was created but never interrupted despite parent cancel-before-materialize",
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    running.booted.shutdown().await;
}

/// Change 1 (#377): receiver-side trusted-paired-peer gate. A cross-deployment
/// bridge (parent authored by a paired peer) must NOT materialize a child when
/// the TARGET behavior's `subagent_allow_cross_deployment` flag is off
/// (default). The trusted-peer branch bypasses `subagent_spawn_denial`, so this
/// gate is enforced separately by reading the target behavior's flag directly.
#[tokio::test]
async fn subagent_source_refuses_cross_deployment_child_when_target_flag_off() {
    let db = test_db("r3-subagent-source-xdep-flag-off").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("r3-xdep-flag-off"));
    let local_did = identity.did().to_string();
    let target_behavior_id = "xdep-target-flag-off";
    let remote_parent_did = "did:key:zPairedPeerParent";

    // Target behavior on THIS node with the cross-deployment flag OFF.
    upsert_target_behavior_with_cross_deployment(
        db.node.as_ref(),
        &local_did,
        target_behavior_id,
        false,
    )
    .await;

    // Parent request authored by the paired-peer (remote) DID.
    let parent_request_id = "r3-parent-xdep-flag-off";
    let parent_session_id = "r3-session-xdep-flag-off";
    let parent_tool_call_id = "r3-tc-xdep-flag-off";
    let child_request_id = "r3-child-xdep-flag-off";
    create_remote_parent_request(
        db.node.as_ref(),
        remote_parent_did,
        parent_request_id,
        parent_session_id,
    )
    .await;

    // Spawn the source FIRST so its global Update subscription is open before the
    // bridge is written (the source has no live rescan; a create event written
    // before subscription would be missed).
    let mut paired = std::collections::HashSet::new();
    paired.insert(remote_parent_did.to_string());
    let _source = crate::support::fixtures::spawn_subagent_source_with_paired_peers(
        db.node.clone(),
        &local_did,
        target_behavior_id,
        target_behavior_id,
        paired,
    );
    wait_for_subagent_source_subscription().await;

    write_cross_deployment_bridge(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
        target_behavior_id,
        remote_parent_did,
    )
    .await;

    // Flag off -> refuse: no child materialized.
    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(800),
    )
    .await;
}

/// Change 1 (#377): with the TARGET behavior's flag ON, the trusted-paired-peer
/// branch proceeds and materializes the cross-deployment child locally.
#[tokio::test]
async fn subagent_source_materializes_cross_deployment_child_when_target_flag_on() {
    let db = test_db("r3-subagent-source-xdep-flag-on").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("r3-xdep-flag-on"));
    let local_did = identity.did().to_string();
    let target_behavior_id = "xdep-target-flag-on";
    let remote_parent_did = "did:key:zPairedPeerParentOn";

    upsert_target_behavior_with_cross_deployment(
        db.node.as_ref(),
        &local_did,
        target_behavior_id,
        true,
    )
    .await;

    let parent_request_id = "r3-parent-xdep-flag-on";
    let parent_session_id = "r3-session-xdep-flag-on";
    let parent_tool_call_id = "r3-tc-xdep-flag-on";
    let child_request_id = "r3-child-xdep-flag-on";
    create_remote_parent_request(
        db.node.as_ref(),
        remote_parent_did,
        parent_request_id,
        parent_session_id,
    )
    .await;

    // Spawn the source FIRST so its subscription is open before the bridge write.
    let mut paired = std::collections::HashSet::new();
    paired.insert(remote_parent_did.to_string());
    let _source = crate::support::fixtures::spawn_subagent_source_with_paired_peers(
        db.node.clone(),
        &local_did,
        target_behavior_id,
        target_behavior_id,
        paired,
    );
    wait_for_subagent_source_subscription().await;

    write_cross_deployment_bridge(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
        target_behavior_id,
        remote_parent_did,
    )
    .await;

    // Flag on -> proceed: child materialized and locally owned.
    let child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_eq!(child.request_id, child_request_id);
    assert_eq!(child.behavior_id, target_behavior_id);
}

/// Change 2 (#377): recovery gate. Recovery must REFUSE to materialize a
/// cross-deployment orphan child when the parent behavior's
/// `subagent_allow_cross_deployment` flag is off, even when the target is
/// otherwise an allowed subagent target.
#[tokio::test]
async fn recovery_refuses_cross_deployment_orphan_when_flag_off() {
    let db = test_db("r3-subagent-source-orphan-xdep-flag-off").await;
    let parent_request_id = "r3-parent-orphan-xdep";
    let parent_session_id = "r3-session-orphan-xdep";
    let parent_tool_call_id = "r3-tc-orphan-xdep";
    let child_request_id = "child-orphan-xdep";
    let remote_target_did = "did:key:zRemoteTargetForRecovery";

    // Authorize a target owned by a REMOTE DID with cross-deployment OFF.
    let selection_id = format!("{}-orphan-xdep-tools", crate::support::AGENT_NAME);
    upsert_tool_selection(
        db.node.as_ref(),
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: crate::support::AGENT_DID.to_string(),
            subagent_targets: Some(vec![defra_agent::subagent_target_entry(
                "remote-recovery-target",
                remote_target_did,
                "remote-recovery-behavior",
                None,
            )]),
            subagent_spawn_enabled: Some(true),
            subagent_background_enabled: Some(true),
            subagent_allow_cross_deployment: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut behavior = match load_agent_behavior(db.node.as_ref(), crate::support::AGENT_NAME)
        .await
        .unwrap()
    {
        Some(behavior) => behavior,
        None => AgentBehaviorDocument {
            behavior_id: crate::support::AGENT_NAME.to_string(),
            agent_did: crate::support::AGENT_DID.to_string(),
            display_name: Some(crate::support::AGENT_NAME.to_string()),
            description: None,
            summary: None,
            system_prompt: None,
            system_context_template: None,
            request_context_template: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
            enabled: true,
            created_at: Some("2026-06-04T00:00:00Z".to_string()),
        },
    };
    behavior.tool_selection_id = Some(selection_id);
    upsert_agent_behavior(db.node.as_ref(), &behavior)
        .await
        .unwrap();

    crate::support::create_request(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        "processing",
        &chrono::Utc::now().to_rfc3339(),
    )
    .await;
    create_orphan_cross_deployment_tool_call(
        db.node.as_ref(),
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
        "remote-recovery-target",
        remote_target_did,
        "remote-recovery-behavior",
    )
    .await;

    let report = ToolCallLifecycle::recover_all(db.node.as_ref(), crate::support::AGENT_DID)
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
    assert_tool_call_not_allowed(&tool, "/name", "remote-recovery-target");
}

/// Change 3 (#377), real race: drive the cascade (parent interrupt) CONCURRENTLY
/// with `SubagentSource` materialization rather than pre-latching before the
/// bridge is written. The bridge is written first (so the source begins
/// materializing), then the parent is interrupted in the same window; the
/// post-create re-check must still interrupt the orphan child.
#[tokio::test]
async fn subagent_source_interrupts_child_on_concurrent_parent_cancel() {
    let db = test_db("r3-subagent-source-orphan-cancel-race").await;
    let running = boot_agent(&db, "r3-subagent-source-orphan-cancel-race").await;
    let parent_request_id = "r3-parent-orphan-cancel-race";
    let parent_tool_call_id = "r3-tc-orphan-cancel-race";
    let child_request_id = "r3-child-orphan-cancel-race";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        "r3-session-orphan-cancel-race",
        "parent prompt",
    )
    .await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "child prompt racing a concurrent parent cancel"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        "r3-session-orphan-cancel-race".to_string(),
        "did:defra-agent:test".to_string(),
        parent_tool_call_id.to_string(),
        1,
        "spawn_subagent".to_string(),
        args,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Background,
        CancelPolicy::Cascade,
        child_request_id.to_string(),
    );

    // Drive the bridge create and the parent interrupt CONCURRENTLY. The source
    // observes the bridge and begins materializing while the cascade latches the
    // parent interrupt. Whichever wins the window, the post-create re-check must
    // converge the orphan child to interrupted.
    let node_for_interrupt = db.node.clone();
    let parent_for_interrupt = parent_request_id.to_string();
    let interrupt_task = tokio::spawn(async move {
        interrupt_request(node_for_interrupt.as_ref(), &parent_for_interrupt)
            .await
            .unwrap();
    });
    lifecycle.start_running().await.unwrap();
    interrupt_task.await.unwrap();

    let _child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if fetch_interrupt_requested_at(db.node.as_ref(), child_request_id)
            .await
            .unwrap()
            .is_some()
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "child created but never interrupted despite concurrent parent cancel",
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    running.booted.shutdown().await;
}

/// Change 3 (#377), parent reached a CANCEL-WORTHY terminal without setting the
/// interrupt latch: the parent reaches `dead`/`error` (NOT clean `completed`) in
/// the materialize window. The cascade only fires on the interrupt latch, and a
/// parent that errored/died would never cascade to a child that did not yet
/// exist. Mirroring the recovery cascade (which drives a Cascade child to
/// `.interrupted` on any cancel-worthy terminal parent), the source's post-create
/// re-check MUST interrupt the just-created Cascade orphan child.
#[tokio::test]
async fn subagent_source_interrupts_cascade_child_when_parent_reaches_dead_terminal() {
    let db = test_db("r3-subagent-source-orphan-parent-dead").await;
    let running = boot_agent(&db, "r3-subagent-source-orphan-parent-dead").await;
    let parent_request_id = "r3-parent-orphan-dead";
    let parent_tool_call_id = "r3-tc-orphan-dead";
    let child_request_id = "r3-child-orphan-dead";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        "r3-session-orphan-dead",
        "parent prompt",
    )
    .await;

    // Terminalize the parent to a CANCEL-WORTHY terminal (dead) WITHOUT setting
    // the interrupt latch. Only the parent-terminal re-check can catch this.
    mark_request_dead(db.node.as_ref(), parent_request_id).await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "child prompt racing a parent death"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        "r3-session-orphan-dead".to_string(),
        "did:defra-agent:test".to_string(),
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

    let _child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if fetch_interrupt_requested_at(db.node.as_ref(), child_request_id)
            .await
            .unwrap()
            .is_some()
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Cascade child created but never interrupted despite dead (cancel-worthy) parent",
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    running.booted.shutdown().await;
}

/// Regression guard for the #377 over-broad re-check: a CASCADE child whose
/// parent completed NORMALLY must NOT be interrupted. A cleanly-completed parent
/// does not cascade-cancel its tools anywhere else (live cascade fires only on
/// `.cancelled`; recovery cascade fires on cancel-worthy terminals, not clean
/// completion). A background subagent of a completed parent must keep running.
#[tokio::test]
async fn subagent_source_does_not_interrupt_cascade_child_when_parent_completed_normally() {
    let db = test_db("r3-subagent-source-cascade-parent-completed").await;
    let running = boot_agent(&db, "r3-subagent-source-cascade-parent-completed").await;
    let parent_request_id = "r3-parent-cascade-completed";
    let parent_tool_call_id = "r3-tc-cascade-completed";
    let child_request_id = "r3-child-cascade-completed";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        "r3-session-cascade-completed",
        "parent prompt",
    )
    .await;

    // Parent completes NORMALLY (no interrupt latch). This is NOT a cancel signal.
    mark_request_completed(db.node.as_ref(), parent_request_id).await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "background child whose parent finished cleanly"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        "r3-session-cascade-completed".to_string(),
        "did:defra-agent:test".to_string(),
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

    let _child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_child_not_interrupted(
        db.node.as_ref(),
        child_request_id,
        Duration::from_millis(800),
    )
    .await;

    running.booted.shutdown().await;
}

/// A DETACHED (cancel_policy != Cascade) child outlives its parent and must NOT
/// be interrupted by the source's post-create re-check, EVEN when the parent is
/// interrupted in the materialize window. Mirrors `bridge_cancel_cascade`
/// (`if self.cancel_policy != CancelPolicy::Cascade { return None } // detached`)
/// and `recovery::is_detached_subagent_tool` (no cascade on interrupt).
#[tokio::test]
async fn subagent_source_does_not_interrupt_detached_child_when_parent_interrupted() {
    let db = test_db("r3-subagent-source-detached-parent-interrupted").await;
    let running = boot_agent(&db, "r3-subagent-source-detached-parent-interrupted").await;
    let parent_request_id = "r3-parent-detached-interrupted";
    let parent_tool_call_id = "r3-tc-detached-interrupted";
    let child_request_id = "r3-child-detached-interrupted";
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        "r3-session-detached-interrupted",
        "parent prompt",
    )
    .await;

    // Latch the parent interrupt BEFORE the bridge — the strongest cancel signal.
    // A Cascade child would be interrupted here; a DETACHED child must survive.
    interrupt_request(db.node.as_ref(), parent_request_id)
        .await
        .unwrap();
    mark_request_interrupted(db.node.as_ref(), parent_request_id).await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "detached child that must outlive an interrupted parent"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        "r3-session-detached-interrupted".to_string(),
        "did:defra-agent:test".to_string(),
        parent_tool_call_id.to_string(),
        1,
        "spawn_subagent".to_string(),
        args,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Background,
        CancelPolicy::Detach,
        child_request_id.to_string(),
    );
    lifecycle.start_running().await.unwrap();

    let _child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_child_not_interrupted(
        db.node.as_ref(),
        child_request_id,
        Duration::from_millis(800),
    )
    .await;

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
        crate::support::AGENT_DID,
        crate::support::AGENT_NAME,
        vec![crate::support::AGENT_NAME.to_string()],
        true,
        true,
    )
    .await;
    crate::support::create_request(
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

    let report = ToolCallLifecycle::recover_all(db.node.as_ref(), crate::support::AGENT_DID)
        .await
        .unwrap();
    assert_eq!(
        report.tool_calls_recovered, 0,
        "orphan backfill should not terminalize a live non-expired tool call"
    );

    let child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_eq!(child.request_id, child_request_id);
    assert_eq!(child.behavior_id, crate::support::AGENT_NAME);
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
        crate::support::AGENT_DID,
        crate::support::AGENT_NAME,
        Vec::new(),
        true,
        true,
    )
    .await;
    crate::support::create_request(
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

    let report = ToolCallLifecycle::recover_all(db.node.as_ref(), crate::support::AGENT_DID)
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
    assert_tool_call_not_allowed(&tool, "/name", crate::support::AGENT_NAME);
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
        crate::support::AGENT_DID,
        crate::support::AGENT_NAME,
        vec![crate::support::AGENT_NAME.to_string()],
        false,
        true,
    )
    .await;
    crate::support::create_request(
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

    let report = ToolCallLifecycle::recover_all(db.node.as_ref(), crate::support::AGENT_DID)
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
        crate::support::AGENT_DID,
        crate::support::AGENT_NAME,
        vec![crate::support::AGENT_NAME.to_string()],
        true,
        false,
    )
    .await;
    crate::support::create_request(
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

    let report = ToolCallLifecycle::recover_all(db.node.as_ref(), crate::support::AGENT_DID)
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

/// The fixture `SubagentSource` opens its global Update subscription lazily on
/// the first `next_fire` poll and has no live rescan, so a bridge written before
/// the subscription is open would be missed. Give the spawned source task a
/// moment to open its subscription before writing the bridge.
async fn wait_for_subagent_source_subscription() {
    tokio::time::sleep(Duration::from_millis(250)).await;
}

/// Settle, then assert the child request was NOT interrupted. Used to verify a
/// detached child (or a Cascade child of a normally-completed parent) is left
/// running by the source's post-create re-check.
async fn assert_child_not_interrupted(
    node: &EmbeddedNode,
    child_request_id: &str,
    settle: Duration,
) {
    tokio::time::sleep(settle).await;
    let interrupt = fetch_interrupt_requested_at(node, child_request_id)
        .await
        .unwrap();
    assert!(
        interrupt.is_none(),
        "child {child_request_id} was unexpectedly interrupted (interrupt_requested_at = {interrupt:?})",
    );
}

async fn mark_request_dead(node: &EmbeddedNode, request_id: &str) {
    let escaped_request_id = escape_graphql_string(request_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                input: {{
                    status: "dead",
                    lifecycle_state: "dead"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "mark_request_dead failed: {:?}",
        response.errors
    );
}

async fn mark_request_completed(node: &EmbeddedNode, request_id: &str) {
    let escaped_request_id = escape_graphql_string(request_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                input: {{
                    status: "completed",
                    lifecycle_state: "completed"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "mark_request_completed failed: {:?}",
        response.errors
    );
}

/// Upsert a target behavior on THIS node with a ToolSelection whose
/// `subagent_allow_cross_deployment` flag is set as requested. The behavior id
/// equals its own allowed target name so the trusted-peer receiver path can
/// resolve the target locally.
async fn upsert_target_behavior_with_cross_deployment(
    node: &EmbeddedNode,
    agent_did: &str,
    target_behavior_id: &str,
    allow_cross_deployment: bool,
) {
    let selection_id = format!("{target_behavior_id}-xdep-tools");
    upsert_tool_selection(
        node,
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: agent_did.to_string(),
            subagent_targets: Some(vec![defra_agent::subagent_target_entry(
                target_behavior_id,
                agent_did,
                target_behavior_id,
                None,
            )]),
            subagent_spawn_enabled: Some(true),
            subagent_background_enabled: Some(true),
            subagent_allow_cross_deployment: Some(allow_cross_deployment),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let behavior = AgentBehaviorDocument {
        behavior_id: target_behavior_id.to_string(),
        agent_did: agent_did.to_string(),
        display_name: Some(target_behavior_id.to_string()),
        description: None,
        summary: None,
        system_prompt: None,
        system_context_template: None,
        request_context_template: None,
        backend_id: None,
        model_name: None,
        tool_selection_id: Some(selection_id),
        inference_profile_id: None,
        compaction_strategy: None,
        compaction_threshold: None,
        skill_refs: Vec::new(),
        skill_excludes: Vec::new(),
        enabled: true,
        created_at: Some("2026-06-04T00:00:00Z".to_string()),
    };
    upsert_agent_behavior(node, &behavior).await.unwrap();
}

/// Write a parent `AgentRequest` authored by a remote (paired-peer) DID, as it
/// would appear after P2P replication from the originating deployment.
async fn create_remote_parent_request(
    node: &EmbeddedNode,
    remote_agent_did: &str,
    parent_request_id: &str,
    parent_session_id: &str,
) {
    let escaped_request_id = escape_graphql_string(parent_request_id);
    let escaped_agent_did = escape_graphql_string(remote_agent_did);
    let escaped_session_id = escape_graphql_string(parent_session_id);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "remote-parent-behavior",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "remote parent prompt",
                status: "processing",
                lifecycle_state: "processing",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 3,
                subagent_depth: 0
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create remote parent AgentRequest failed: {:?}",
        response.errors
    );
}

/// Write a running `spawn_subagent` bridge whose args carry the RESOLVED target
/// `(name, agent_did, behavior_id)` for a cross-deployment spawn, as the spawn
/// hook on the originating node would write before replication.
#[allow(clippy::too_many_arguments)]
async fn write_cross_deployment_bridge(
    node: &EmbeddedNode,
    parent_request_id: &str,
    parent_session_id: &str,
    parent_tool_call_id: &str,
    child_request_id: &str,
    target_behavior_id: &str,
    target_agent_did: &str,
) {
    let escaped_parent_request_id = escape_graphql_string(parent_request_id);
    let escaped_parent_session_id = escape_graphql_string(parent_session_id);
    let escaped_parent_tool_call_id = escape_graphql_string(parent_tool_call_id);
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let tool_call_key = format!("{escaped_parent_session_id}:{escaped_parent_tool_call_id}");
    let args = serde_json::json!({
        "name": target_behavior_id,
        "agent_did": target_agent_did,
        "behavior_id": target_behavior_id,
        "prompt": "cross-deployment child prompt"
    })
    .to_string();
    let escaped_args = escape_graphql_string(&args);
    let started_at = chrono::Utc::now().to_rfc3339();
    let deadline_at = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
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
                await_mode: "background",
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
        "create cross-deployment AgentToolCall failed: {:?}",
        response.errors
    );
}

/// Write a running orphan `spawn_subagent` bridge whose args carry a RESOLVED
/// REMOTE target `(name, agent_did, behavior_id)`, used by the recovery gate
/// test.
#[allow(clippy::too_many_arguments)]
async fn create_orphan_cross_deployment_tool_call(
    node: &EmbeddedNode,
    parent_request_id: &str,
    parent_session_id: &str,
    parent_tool_call_id: &str,
    child_request_id: &str,
    target_name: &str,
    target_agent_did: &str,
    target_behavior_id: &str,
) {
    let escaped_parent_request_id = escape_graphql_string(parent_request_id);
    let escaped_parent_session_id = escape_graphql_string(parent_session_id);
    let escaped_parent_tool_call_id = escape_graphql_string(parent_tool_call_id);
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let tool_call_key = format!("{escaped_parent_session_id}:{escaped_parent_tool_call_id}");
    let args = serde_json::json!({
        "name": target_name,
        "agent_did": target_agent_did,
        "behavior_id": target_behavior_id,
        "prompt": "orphan cross-deployment child prompt"
    })
    .to_string();
    let escaped_args = escape_graphql_string(&args);
    let started_at = chrono::Utc::now().to_rfc3339();
    let deadline_at = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
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
                await_mode: "background",
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
        "create orphan cross-deployment AgentToolCall failed: {:?}",
        response.errors
    );
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
        "behavior_id": crate::support::AGENT_NAME,
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
