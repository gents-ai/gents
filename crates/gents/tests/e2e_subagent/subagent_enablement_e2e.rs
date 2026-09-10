use std::sync::Arc;
use std::time::Duration;

use gents::__test_internals::{handle_list_subagents, ListSubagentsArgs};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};
use gents::{
    default_behavior_id_for_agent, AgentIdentity, DocumentRuntimeOptions, Gents, ToolCeiling,
};
use gents_protocol::row::AgentRequestRow;

use crate::support::fixtures::{
    bind_default_behavior_backend, configure_subagent_behavior, subagent_target, test_identity,
};
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::{first_optional_row, test_db};

struct RunningAgent {
    booted: BootedAgent,
    _endpoint: MockModelEndpoint,
    behavior_id: String,
}

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

    let selection_id = format!("{behavior_id}-e2e-spawn-tools");
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        &selection_id,
        vec![subagent_target(
            &agent_did,
            behavior_id.clone(),
            agent_did.clone(),
            behavior_id.clone(),
        )],
        true,
        true,
        None,
    )
    .await;

    let agent = Gents::from_default_behavior_documents(
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

async fn wait_for_child_request(node: &EmbeddedNode, child_request_id: &str) -> AgentRequestRow {
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
        if let Some(row) = first_optional_row::<AgentRequestRow>(&response, "AgentRequest") {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for child AgentRequest {child_request_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn enabled_agent_spawns_local_child_and_list_reflects_it() {
    let db = test_db("e2e-subagent-enablement").await;
    let running = boot_self_spawn_agent(&db, "e2e-subagent-enablement").await;

    let parent_request_id = "e2e-parent-list";
    let parent_session_id = "e2e-session-list";
    let parent_tool_call_id = "e2e-tc-list";
    let child_request_id = "e2e-child-list";

    create_runtime_request(
        db.node.as_ref(),
        &running.booted.agent_did,
        &running.behavior_id,
        parent_request_id,
        parent_session_id,
        "parent prompt for list test",
    )
    .await;
    let parent_request_doc_id =
        crate::support::exact_request_doc_id(db.node.as_ref(), parent_request_id).await;

    let args = serde_json::json!({
        "name": running.behavior_id.clone(),
        "agent_did": running.booted.agent_did.clone(),
        "behavior_id": running.behavior_id.clone(),
        "prompt": "child work",
        "await_mode": "background",
        "parent_subagent_depth": 0
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        parent_session_id.to_string(),
        running.booted.agent_did.clone(),
        parent_tool_call_id.to_string(),
        1,
        "spawn_subagent".to_string(),
        args,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Background,
        CancelPolicy::Cascade,
        child_request_id.to_string(),
        running.booted.agent_did.clone(),
    )
    .with_request_doc_id(Some(parent_request_doc_id))
    .with_requester_did(Some(running.booted.agent_did.clone()));
    lifecycle.start_running().await.unwrap();

    let child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_eq!(
        child.request_id, child_request_id,
        "child request_id must match"
    );
    assert_eq!(
        child.behavior_id.as_deref(),
        Some(running.behavior_id.as_str()),
        "child behavior_id must match parent behavior"
    );

    let resp = handle_list_subagents(
        db.node.as_ref(),
        parent_request_id,
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
        entry.behavior_id.as_deref(),
        Some(running.behavior_id.as_str()),
        "entry behavior_id must match"
    );

    running.booted.shutdown().await;
}
