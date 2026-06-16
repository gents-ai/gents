//! Characterization tests for the local/remote subagent spawn convergence
//! refactor (#377).
//!
//! These capture CURRENT observable behavior of the local spawn path against a
//! full running agent (which boots `SubagentSource`). They are the regression
//! safety net for converging the same-deployment (local) and cross-deployment
//! (remote) spawn paths into one "write the bridge, let SubagentSource create
//! the child" path.
//!
//! Harness mirrors `subagent_enablement_e2e.rs`: a real `DefraAgent::run`
//! drives the `TriggerEngine` + `SubagentSource`, and spawns are driven by
//! writing the `AgentToolCall` bridge via `ToolCallLifecycle::start_running()`
//! (exactly how the hook writes it before its local/remote branch).

use std::sync::Arc;
use std::time::Duration;

use defra_agent::__test_internals::{handle_list_subagents, ListSubagentsArgs};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};
use defra_agent::{
    default_behavior_id_for_agent, load_agent_behavior, upsert_agent_behavior,
    upsert_tool_selection, AgentBehaviorDocument, AgentIdentity, DefraAgent,
    DocumentRuntimeOptions, ToolCeiling, ToolSelectionDocument,
};
use serde::Deserialize;

use crate::support::fixtures::{bind_default_behavior_backend, test_identity};
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::{first_optional_row, test_db};

struct RunningAgent {
    booted: BootedAgent,
    _endpoint: MockModelEndpoint,
    behavior_id: String,
}

/// Boot an agent with spawn_enabled + background_enabled and `behavior_id`
/// as its own allowed subagent target (local self-spawn).
async fn boot_self_spawn_agent(db: &crate::support::TestDb, test_name: &str) -> RunningAgent {
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity(test_name));
    let agent_did = identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&agent_did);

    let endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        &agent_did,
        "backend-convergence",
        endpoint.endpoint(),
    )
    .await;

    let selection_id = format!("{behavior_id}-convergence-spawn-tools");
    upsert_tool_selection(
        db.node.as_ref(),
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: agent_did.clone(),
            subagent_targets: Some(vec![defra_agent::subagent_target_entry(
                behavior_id.clone(),
                &agent_did,
                behavior_id.clone(),
                None,
            )]),
            subagent_spawn_enabled: Some(true),
            subagent_background_enabled: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let mut behavior = match load_agent_behavior(db.node.as_ref(), &behavior_id)
        .await
        .unwrap()
    {
        Some(b) => b,
        None => AgentBehaviorDocument {
            behavior_id: behavior_id.clone(),
            agent_did: agent_did.clone(),
            display_name: Some(behavior_id.clone()),
            description: None,
            summary: None,
            system_prompt: None,
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
    let escaped = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
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

#[derive(Debug, Deserialize)]
struct ToolCallStateRow {
    lifecycle_state: Option<String>,
}

async fn fetch_tool_call_state(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Option<String> {
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
            ) {{ lifecycle_state }}
        }}"#
    );
    first_optional_row::<ToolCallStateRow>(&node.execute(&query).await, "AgentToolCall")
        .and_then(|row| row.lifecycle_state)
}

/// CHARACTERIZATION: a local BACKGROUND spawn driven by writing the bridge
/// materializes a child `AgentRequest` (via SubagentSource) with correct
/// lineage, and `handle_list_subagents` reflects the running child.
#[tokio::test]
async fn local_background_spawn_materializes_child_with_lineage_and_lists() {
    let db = test_db("convergence-local-background").await;
    let running = boot_self_spawn_agent(&db, "convergence-local-background").await;

    let parent_request_id = "convergence-bg-parent";
    let parent_session_id = "convergence-bg-session";
    let parent_tool_call_id = "convergence-bg-tc";
    let child_request_id = "convergence-bg-child";

    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        parent_session_id,
        "parent prompt background",
    )
    .await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "background child work",
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

    let child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_eq!(child.request_id, child_request_id);
    assert_eq!(child.behavior_id, running.behavior_id);
    assert_eq!(child.content, "background child work");
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

    let resp = handle_list_subagents(
        db.node.as_ref(),
        parent_request_id,
        &running.booted.agent_did,
        ListSubagentsArgs::default(),
    )
    .await
    .expect("handle_list_subagents must not error");
    let entry = resp
        .entries
        .iter()
        .find(|e| e.child_request_id == child_request_id)
        .expect("list_subagents must reflect the running background child");
    assert_eq!(entry.behavior_id, running.behavior_id);

    running.booted.shutdown().await;
}

/// CHARACTERIZATION: a local FOREGROUND spawn driven by writing the bridge
/// materializes a child `AgentRequest` (via SubagentSource) with correct
/// lineage. The foreground bridge stays `running` until the child reaches a
/// terminal state.
///
/// NOTE: this characterizes the spawn at the level reachable without the model
/// loop. We assert (a) the child materializes via SubagentSource for a local
/// foreground bridge and (b) the bridge is observable as `running` after the
/// child exists, then (c) bridge completion projects to the parent tool call.
/// We do NOT exercise the hook's blocking `await_foreground_subagent` poll here
/// (that requires the model loop); the convergence refactor relies on
/// SubagentSource creating the child for both await modes, which this captures.
#[tokio::test]
async fn local_foreground_spawn_materializes_child_via_source() {
    let db = test_db("convergence-local-foreground").await;
    let running = boot_self_spawn_agent(&db, "convergence-local-foreground").await;

    let parent_request_id = "convergence-fg-parent";
    let parent_session_id = "convergence-fg-session";
    let parent_tool_call_id = "convergence-fg-tc";
    let child_request_id = "convergence-fg-child";

    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        parent_session_id,
        "parent prompt foreground",
    )
    .await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "foreground child work"
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

    // SubagentSource creates the child for a local foreground bridge too.
    let child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_eq!(child.request_id, child_request_id);
    assert_eq!(child.behavior_id, running.behavior_id);
    assert_eq!(child.content, "foreground child work");
    assert_eq!(child.subagent_depth, Some(1));
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(parent_request_id)
    );
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        Some(parent_tool_call_id)
    );

    // The foreground bridge is still running while the child is live.
    let state = fetch_tool_call_state(db.node.as_ref(), parent_session_id, parent_tool_call_id)
        .await
        .expect("bridge tool call row must exist");
    assert_eq!(state.as_str(), "running");

    // Driving the bridge to completion projects to the parent tool call.
    lifecycle
        .bridge_complete("foreground final answer".to_string())
        .await
        .unwrap();
    let state = fetch_tool_call_state(db.node.as_ref(), parent_session_id, parent_tool_call_id)
        .await
        .expect("bridge tool call row must exist after completion");
    assert_eq!(state.as_str(), "completed");

    running.booted.shutdown().await;
}
