//! Tier-1 end-to-end test: enabled agent -> spawn local background child ->
//! list_subagents reflects it.
//!
//! Validates the subagent enablement runtime path and serves as a regression
//! anchor for C2 state (running-subagent listing completeness).

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

// ── Minimal agent boot helper (mirrors subagent_source_conformance.rs) ──────

struct RunningAgent {
    booted: BootedAgent,
    _endpoint: MockModelEndpoint,
    behavior_id: String,
}

/// Boot an agent with spawn_enabled + background_enabled and `behavior_id`
/// as its own allowed subagent target (self-spawn).
async fn boot_self_spawn_agent(db: &crate::support::TestDb, test_name: &str) -> RunningAgent {
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity(test_name));
    let agent_did = identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&agent_did);

    let endpoint = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db.node.as_ref(),
        &agent_did,
        "backend-e2e-enablement",
        endpoint.endpoint(),
    )
    .await;

    // Allow behavior to spawn itself as a subagent with background mode.
    let selection_id = format!("{behavior_id}-e2e-spawn-tools");
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
            created_at: Some("2026-05-22T00:00:00Z".to_string()),
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

// ── Poll helper ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ChildRequestRow {
    request_id: String,
    behavior_id: String,
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

// ── Test ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn enabled_agent_spawns_local_child_and_list_reflects_it() {
    let db = test_db("e2e-subagent-enablement").await;
    let running = boot_self_spawn_agent(&db, "e2e-subagent-enablement").await;

    let parent_request_id = "e2e-parent-list";
    let parent_session_id = "e2e-session-list";
    let parent_tool_call_id = "e2e-tc-list";
    let child_request_id = "e2e-child-list";

    // Create the parent request that the tool call will be linked to.
    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        parent_session_id,
        "parent prompt for list test",
    )
    .await;

    // Drive a BACKGROUND spawn directly via ToolCallLifecycle (no model scripting required).
    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "child work",
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

    // Assert child AgentRequest materializes with correct lineage.
    let child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_eq!(
        child.request_id, child_request_id,
        "child request_id must match"
    );
    assert_eq!(
        child.behavior_id, running.behavior_id,
        "child behavior_id must match parent behavior"
    );

    // Assert handle_list_subagents reflects the running background child.
    // This is the C2 completeness assertion: the enabled path must be
    // end-to-end queryable.
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
        .find(|e| e.child_request_id == child_request_id);

    assert!(
        entry.is_some(),
        "list_subagents must contain child_request_id={child_request_id}; \
         got {} entries: {:?}",
        resp.entries.len(),
        resp.entries
            .iter()
            .map(|e| &e.child_request_id)
            .collect::<Vec<_>>()
    );

    let entry = entry.expect("checked Some above");
    assert_eq!(
        entry.await_mode, "background",
        "entry await_mode must be background"
    );
    assert_eq!(
        entry.behavior_id, running.behavior_id,
        "entry behavior_id must match"
    );

    running.booted.shutdown().await;
}
