use std::sync::Arc;
use std::time::Duration;

use gents::__test_internals::{handle_list_subagents, ListSubagentsArgs};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{AgentIdentity, DocumentRuntimeOptions, Gents, ToolCeiling};
use gents_protocol::row::AgentRequestRow;
use serde_json::json;

use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_accepted_turn, AcceptedTurnSpec,
};
use crate::support::fixtures::{configure_subagent_behavior, subagent_target};
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse, StreamScript};
use crate::support::{exact_request_doc_id, first_optional_row, test_db};

const PARENT_BEHAVIOR_ID: &str = "e2e-enablement-parent";
const CHILD_BEHAVIOR_ID: &str = "e2e-enablement-child";
const BACKEND_ID: &str = "e2e-enablement-backend";
const MODEL: &str = "e2e-enablement-model";

/// Parent request id. It is part of the marker the streaming backend matches,
/// so it is chosen here once and reused for both the spec and the wait loop.
const PARENT_REQUEST_ID: &str = "e2e-enablement-parent-request";
const PARENT_SESSION_ID: &str = "e2e-enablement-parent-session";
const PARENT_TOOL_CALL_ID: &str = "e2e-enablement-parent-tool-call";
const CHILD_PROMPT: &str = "e2e-enablement-child-prompt";
const PARENT_PROMPT: &str = "e2e-enablement-parent-prompt";

async fn configure_self_spawn_chain(db: &crate::support::TestDb) {
    let agent_did = db.node_identity.did().to_string();

    // The child behavior document exists so the parent's spawn target
    // resolves, and the child runtime later executes the child request
    // against the same shared backend.
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        CHILD_BEHAVIOR_ID,
        "e2e-enablement-child-tools",
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        PARENT_BEHAVIOR_ID,
        "e2e-enablement-parent-tools",
        vec![subagent_target(
            &agent_did,
            CHILD_BEHAVIOR_ID,
            &agent_did,
            CHILD_BEHAVIOR_ID,
        )],
        true,
        true,
        None,
    )
    .await;
}

async fn wait_for_child_request_terminal(node: &EmbeddedNode, request_id: &str, expected: &str) {
    let escaped = escape_graphql_string(request_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state failure_reason }} }}"#
            ))
            .await;
        let row = response.data.as_ref().and_then(|data| {
            data["AgentRequest"]
                .as_array()
                .and_then(|rows| rows.first())
        });
        let state = row.and_then(|row| row["lifecycle_state"].as_str());
        if state == Some(expected) {
            return;
        }
        assert_ne!(
            state,
            Some("failed"),
            "child request failed: {}",
            row.and_then(|row| row["failure_reason"].as_str())
                .unwrap_or("missing failure reason")
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for child AgentRequest {request_id} state={state:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn fetch_child_row(node: &EmbeddedNode, parent_request_id: &str) -> Option<AgentRequestRow> {
    let escaped = escape_graphql_string(parent_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_parent_request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                request_id
                behavior_id
                session_id
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
                agent_did
                requester_did
            }}
        }}"#
    );
    first_optional_row::<AgentRequestRow>(&node.execute(&query).await, "AgentRequest")
}

async fn wait_for_child_row(
    node: &EmbeddedNode,
    child_request_id: &str,
    backend: &crate::support::streaming_backend::MockStreamingBackend,
) -> AgentRequestRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(row) = fetch_child_row(node, child_request_id).await {
            return row;
        }
        if tokio::time::Instant::now() >= deadline {
            let diagnostic = node.execute(
                "{ AgentRequest { request_id lifecycle_state failure_reason } AgentToolCall { tool_call_id tool_name lifecycle_state child_request_id denial_reason } }"
            ).await;
            panic!("timed out waiting for child of {child_request_id}: {diagnostic:?}; provider bodies: {:?}; matched chunks: {}", backend.observed_completion_bodies(), backend.observed_chunks(PARENT_PROMPT));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn fetch_parent_tool_call_row(
    node: &EmbeddedNode,
    session_id: &str,
    tool_doc_id: &str,
) -> serde_json::Value {
    let session_id = escape_graphql_string(session_id);
    let tool_doc_id = escape_graphql_string(tool_doc_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    _docID: {{ _eq: "{tool_doc_id}" }}
                }},
                limit: 1
            ) {{
                _docID
                tool_call_id
                agent_did
                requester_did
                await_mode
                cancel_policy
                child_request_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "parent AgentToolCall query failed: {:?}",
        response.errors
    );
    response.data.expect("parent tool call data")["AgentToolCall"][0].clone()
}

#[tokio::test]
async fn enabled_agent_spawns_local_child_and_list_reflects_it() {
    let db = test_db("e2e-subagent-enablement").await;
    configure_self_spawn_chain(&db).await;

    // One accepted provider turn publishes a spawn_subagent tool call; the
    // runtime accepts it through the owned loop and the owned bridge creates
    // the child. The child plan is paused so the child stays running while
    // the list is read.
    let runtime = {
        let prepared = prepare_accepted_turn(
            &db,
            AcceptedTurnSpec {
                backend_id: BACKEND_ID,
                model: MODEL,
                parent_behavior_id: PARENT_BEHAVIOR_ID,
                configured_behavior_ids: &[PARENT_BEHAVIOR_ID, CHILD_BEHAVIOR_ID],
                request_id: PARENT_REQUEST_ID,
                session_id: PARENT_SESSION_ID,
                prompt: PARENT_PROMPT,
                accepted_chunks: vec![StreamChunk::tool_call(
                    PARENT_TOOL_CALL_ID,
                    "spawn_subagent",
                    json!({
                        "name": CHILD_BEHAVIOR_ID,
                        "prompt": CHILD_PROMPT,
                        "await_mode": "background"
                    })
                    .to_string(),
                )],
                child_plans: vec![StreamPlan::new(
                    CHILD_PROMPT,
                    vec![StreamResponse::Stream(StreamScript::paused(
                        CHILD_PROMPT,
                        ["child started"],
                    ))],
                )],
                valid_until: None,
                subagent_depth: Some(0),
                request_setup: None,
            },
        )
        .await;
        boot_prepared_accepted_turn(
            &db,
            prepared,
            Gents::from_default_behavior_documents(
                db.node.clone(),
                {
                    let identity: Arc<dyn AgentIdentity> = db.node_identity.clone();
                    identity
                },
                DocumentRuntimeOptions {
                    tool_ceiling: ToolCeiling::meta_only(),
                    ..Default::default()
                },
            )
            .await
            .expect("build accepted-turn runtime"),
        )
        .await
    };

    // Observe the child the runtime actually created: the physical parent
    // request document and tool-call document are the only lineage sources.
    let child = wait_for_child_row(db.node.as_ref(), PARENT_REQUEST_ID, &runtime.backend).await;
    let child_request_id = child.request_id.as_str();
    let parent_request_doc_id = exact_request_doc_id(db.node.as_ref(), PARENT_REQUEST_ID).await;
    let tool_call = fetch_parent_tool_call_row(
        db.node.as_ref(),
        PARENT_SESSION_ID,
        child
            .caused_by_parent_tool_call_doc_id
            .as_deref()
            .expect("runtime child tool document"),
    )
    .await;
    let tool_call_doc_id = tool_call["_docID"]
        .as_str()
        .expect("parent tool call physical identity")
        .to_string();

    assert_eq!(
        child.behavior_id.as_deref(),
        Some(CHILD_BEHAVIOR_ID),
        "child behavior_id must match the spawn target"
    );
    assert_eq!(
        child.agent_did.as_deref(),
        Some(db.node_identity.did()),
        "child agent_did must match the spawn target's agent"
    );
    assert_eq!(
        child.requester_did.as_deref(),
        Some(db.node_identity.did()),
        "child requester_did must match the parent's requester"
    );
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(PARENT_REQUEST_ID),
        "child caused_by_parent_request_id must match parent request_id"
    );
    assert_eq!(
        child.caused_by_parent_request_doc_id.as_deref(),
        Some(parent_request_doc_id.as_str()),
        "child caused_by_parent_request_doc_id must match parent request doc"
    );
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        tool_call["tool_call_id"].as_str(),
        "child caused_by_parent_tool_call_id must match parent tool_call_id"
    );
    assert_eq!(
        child.caused_by_parent_tool_call_doc_id.as_deref(),
        Some(tool_call_doc_id.as_str()),
        "child caused_by_parent_tool_call_doc_id must match the runtime-created AgentToolCall doc"
    );
    assert_eq!(child.subagent_depth, Some(1));

    // Preserve the list-subagents assertions: the runtime child must be
    // visible through handle_list_subagents with the original fields.
    let resp = handle_list_subagents(&db.node, PARENT_REQUEST_ID, ListSubagentsArgs::default())
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
        Some(CHILD_BEHAVIOR_ID),
        "entry behavior_id must match"
    );

    // The child keeps consuming real provider output: release its paused plan
    // and wait for a terminal provider turn.
    runtime.backend.release(CHILD_PROMPT);
    wait_for_child_request_terminal(db.node.as_ref(), child_request_id, "completed").await;

    runtime.shutdown().await;
}
