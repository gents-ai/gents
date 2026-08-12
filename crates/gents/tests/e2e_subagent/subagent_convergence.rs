use std::sync::Arc;
use std::time::Duration;

use gents::__test_internals::{
    handle_list_subagents, handle_read_subagent, load_steer_subagent_target, ListSubagentsArgs,
    ReadSubagentArgs, SteerSubagentTarget, AWAITING_CHILD_MATERIALIZATION,
};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};
use gents::{
    default_behavior_id_for_agent, load_agent_behavior, upsert_agent_behavior,
    upsert_tool_selection, AgentBehaviorDocument, AgentIdentity, DocumentRuntimeOptions, Gents,
    ToolCeiling, ToolSelectionDocument,
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
            subagent_targets: Some(vec![gents::subagent_target_entry(
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
struct RequestDeadlineRow {
    deadline: Option<String>,
}

async fn wait_for_request_deadline(node: &EmbeddedNode, request_id: &str) {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{ deadline }}
        }}"#
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let response = node.execute(&query).await;
        if let Some(row) = first_optional_row::<RequestDeadlineRow>(&response, "AgentRequest") {
            if row.deadline.is_some_and(|value| !value.trim().is_empty()) {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for request {request_id} to be claimed (deadline stamped)"
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
    let parent_request_doc_id =
        crate::support::exact_request_doc_id(db.node.as_ref(), parent_request_id).await;

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
        "did:test:test".to_string(),
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
    .with_request_doc_id(Some(parent_request_doc_id));
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

#[tokio::test]
async fn unmaterialized_background_child_stays_observable_in_list() {
    let db = test_db("convergence-unmaterialized").await;
    let running = boot_self_spawn_agent(&db, "convergence-unmaterialized").await;

    let parent_request_id = "unmat-bg-parent";
    let parent_session_id = "unmat-bg-session";
    let parent_tool_call_id = "unmat-bg-tc";
    let child_request_id = "unmat-bg-child";
    let agent_did = running.booted.agent_did.clone();

    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &running.behavior_id,
        parent_request_id,
        parent_session_id,
        "parent prompt unmaterialized",
    )
    .await;
    wait_for_request_deadline(db.node.as_ref(), parent_request_id).await;
    let parent_request_doc_id =
        crate::support::exact_request_doc_id(db.node.as_ref(), parent_request_id).await;

    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &running.behavior_id,
        "unmat-other-parent",
        "unmat-other-session",
        "unrelated parent prompt",
    )
    .await;
    wait_for_request_deadline(db.node.as_ref(), "unmat-other-parent").await;

    // This case exercises bridge-level observability in isolation. Stop the
    // source before inserting the deliberately unconfigured remote target so
    // it cannot concurrently reject the synthetic bridge as unauthorized.
    running.booted.shutdown().await;

    let args = serde_json::json!({
        "name": "remote-coder",
        "agent_did": "did:key:z6MkUnclaimedRemoteTarget",
        "behavior_id": "remote-coder-behavior",
        "prompt": "cross-deployment child work",
        "await_mode": "background"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        parent_session_id.to_string(),
        "did:test:test".to_string(),
        parent_tool_call_id.to_string(),
        1,
        "spawn_subagent".to_string(),
        args,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Background,
        CancelPolicy::Cascade,
        child_request_id.to_string(),
        "did:key:z6MkUnclaimedRemoteTarget".to_string(),
    )
    .with_request_doc_id(Some(parent_request_doc_id));
    lifecycle.start_running().await.unwrap();

    let escaped_child = escape_graphql_string(child_request_id);
    let child_query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_child}" }} }}, limit: 1) {{ request_id behavior_id content subagent_depth caused_by_parent_request_id caused_by_parent_tool_call_id caused_by_trigger_id caused_by_trigger_kind }} }}"#
    );
    let child_row =
        first_optional_row::<ChildRequestRow>(&db.node.execute(&child_query).await, "AgentRequest");
    assert!(
        child_row.is_none(),
        "test premise: remote-target child must not materialize locally"
    );

    let all: ListSubagentsArgs =
        serde_json::from_value(serde_json::json!({ "status": "all" })).unwrap();
    let resp = handle_list_subagents(db.node.as_ref(), parent_request_id, &agent_did, all)
        .await
        .expect("handle_list_subagents must not error");
    let entry = resp
        .entries
        .iter()
        .find(|e| e.child_request_id == child_request_id)
        .expect("a returned background child id must never disappear from list_subagents(all)");
    assert_eq!(entry.status, AWAITING_CHILD_MATERIALIZATION);
    assert_eq!(entry.status, "awaiting_child_materialization");
    assert_eq!(entry.await_mode, "background");
    assert_eq!(entry.name, "remote-coder");
    assert_eq!(entry.behavior_id, "remote-coder-behavior");
    assert!(
        entry.child_session_id.is_empty(),
        "no session exists until the child materializes"
    );
    let list_diagnostic = entry
        .diagnostic
        .as_deref()
        .expect("bridge-level entry must carry a diagnostic");
    assert!(
        list_diagnostic.contains(parent_tool_call_id),
        "diagnostic names the bridge: {list_diagnostic}"
    );

    let resp = handle_list_subagents(
        db.node.as_ref(),
        parent_request_id,
        &agent_did,
        ListSubagentsArgs::default(),
    )
    .await
    .expect("handle_list_subagents must not error");
    assert!(
        resp.entries
            .iter()
            .any(|e| e.child_request_id == child_request_id),
        "the unmaterialized handle must be visible under the default running filter"
    );

    let read_args: ReadSubagentArgs =
        serde_json::from_value(serde_json::json!({ "child_request_id": child_request_id }))
            .unwrap();
    let read = handle_read_subagent(db.node.as_ref(), parent_request_id, read_args)
        .await
        .expect("handle_read_subagent must not error")
        .expect("the bridge-level handle must be readable before materialization");
    assert!(!read.terminal);
    assert_eq!(read.lifecycle_state, AWAITING_CHILD_MATERIALIZATION);
    assert!(read.transcript.is_empty());
    assert!(read.child_session_id.is_empty());
    assert!(!read.has_more);
    let read_diagnostic = read
        .diagnostic
        .as_deref()
        .expect("bridge-state read must carry a diagnostic");
    assert!(
        read_diagnostic.contains(parent_tool_call_id),
        "diagnostic names the bridge: {read_diagnostic}"
    );

    match load_steer_subagent_target(db.node.as_ref(), parent_request_id, child_request_id)
        .await
        .expect("load_steer_subagent_target must not error")
    {
        SteerSubagentTarget::AwaitingMaterialization { message } => {
            assert!(
                message.contains(child_request_id),
                "steer explanation names the child: {message}"
            );
        }
        other => panic!("expected AwaitingMaterialization, got {other:?}"),
    }

    let stranger = handle_read_subagent(
        db.node.as_ref(),
        "unmat-other-parent",
        serde_json::from_value(serde_json::json!({ "child_request_id": child_request_id }))
            .unwrap(),
    )
    .await
    .expect("handle_read_subagent must not error");
    assert!(
        stranger.is_none(),
        "a non-owning caller must not see the bridge-level handle"
    );
}

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
    let parent_request_doc_id =
        crate::support::exact_request_doc_id(db.node.as_ref(), parent_request_id).await;

    let args = serde_json::json!({
        "behavior_id": running.behavior_id.clone(),
        "prompt": "foreground child work"
    })
    .to_string();
    let mut lifecycle = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        parent_request_id.to_string(),
        parent_session_id.to_string(),
        "did:test:test".to_string(),
        parent_tool_call_id.to_string(),
        1,
        "spawn_subagent".to_string(),
        args,
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        child_request_id.to_string(),
        running.booted.agent_did.clone(),
    )
    .with_request_doc_id(Some(parent_request_doc_id));
    lifecycle.start_running().await.unwrap();

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

    let state = fetch_tool_call_state(db.node.as_ref(), parent_session_id, parent_tool_call_id)
        .await
        .expect("bridge tool call row must exist");
    assert_eq!(state.as_str(), "running");

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
