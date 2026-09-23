//! The native capacity-one foreground chain: one retained parent, one live child.

use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{default_behavior_id_for_agent, DocumentRuntimeOptions, ToolCeiling};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::{AgentRequestRow, AgentToolCallRow};

use crate::support::accepted_turn::{boot_accepted_turn, AcceptedTurnSpec};
use crate::support::fixtures::{configure_subagent_behavior, subagent_target};
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse, StreamScript};
use crate::support::{first_optional_row, first_row, test_db};

async fn request_by_id(node: &EmbeddedNode, request_id: &str) -> Option<AgentRequestRow> {
    let request_id = escape_graphql_string(request_id);
    let response = node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{ _docID request_id behavior_id lifecycle_state caused_by_parent_request_id caused_by_parent_tool_call_doc_id }} }}"#,
    )).await;
    first_optional_row(&response, "AgentRequest")
}

async fn bridge_by_call(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Option<AgentToolCallRow> {
    let session_id = escape_graphql_string(session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let response = node.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }}, tool_call_id: {{ _eq: "{tool_call_id}" }} }}, limit: 1) {{ _docID tool_call_key tool_call_id lifecycle_state child_request_id await_mode }} }}"#,
    )).await;
    first_optional_row(&response, "AgentToolCall")
}

#[tokio::test]
async fn capacity_one_same_behavior_foreground_child_completes_and_parent_resumes() {
    let db = test_db("same-behavior-foreground-capacity-one").await;
    let agent_did = db.node_identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&agent_did);
    let backend_id = "same-behavior-capacity-one-backend";
    let parent_request_id = "same-behavior-capacity-one-parent";
    let parent_session_id = "same-behavior-capacity-one-session";
    let tool_call_id = "same-behavior-capacity-one-spawn";
    let child_prompt = "same behavior child capacity one";

    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        "same-behavior-capacity-one-tools",
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
    let arguments = serde_json::json!({
        "name": behavior_id,
        "prompt": child_prompt,
        "await_mode": "foreground",
        "deadline": (chrono::Utc::now() + chrono::Duration::minutes(5))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    })
    .to_string();
    let turn = boot_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id,
            model: "same-behavior-capacity-one-model",
            parent_behavior_id: &behavior_id,
            configured_behavior_ids: &[&behavior_id],
            request_id: parent_request_id,
            session_id: parent_session_id,
            prompt: "same behavior parent capacity one",
            accepted_chunks: vec![StreamChunk::tool_call(
                tool_call_id,
                "spawn_subagent",
                arguments,
            )],
            child_plans: vec![StreamPlan::new(
                child_prompt,
                vec![StreamResponse::Stream(StreamScript::paused(
                    child_prompt,
                    ["child running"],
                ))],
            )],
            valid_until: None,
            subagent_depth: Some(0),
            request_setup: None,
        },
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await;

    // The accepted-turn fixture configures exactly one provider slot. The
    // local request-worker capacity is independent but has the same active
    // limit; the retained parent must release it before this child can run.
    let backend_id_escaped = escape_graphql_string(backend_id);
    let backend = db.node.execute(&format!(
        r#"{{ InferenceBackend(filter: {{ backend_id: {{ _eq: "{backend_id_escaped}" }} }}, limit: 1) {{ max_concurrent }} }}"#,
    )).await;
    assert_eq!(
        first_row::<serde_json::Value>(&backend, "InferenceBackend")["max_concurrent"],
        1,
        "the native parent/child chain must share a capacity-one backend"
    );

    let child = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let parent_request_id = escape_graphql_string(parent_request_id);
            let response = db.node.execute(&format!(
                r#"{{ AgentRequest(filter: {{ caused_by_parent_request_id: {{ _eq: "{parent_request_id}" }} }}, limit: 2) {{ _docID request_id behavior_id lifecycle_state caused_by_parent_request_id caused_by_parent_tool_call_doc_id }} }}"#,
            )).await;
            if let Some(child) = first_optional_row::<AgentRequestRow>(&response, "AgentRequest") {
                if child.lifecycle_state == Some(RequestLifecycleState::Processing) {
                    break child;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("same-behavior child must enter processing while parent waits");
    assert_eq!(child.behavior_id.as_deref(), Some(behavior_id.as_str()));
    let child_request_id = child.request_id.clone();
    let parent = request_by_id(db.node.as_ref(), parent_request_id)
        .await
        .expect("parent persists while child runs");
    assert_eq!(
        parent.lifecycle_state,
        Some(RequestLifecycleState::Processing)
    );
    let bridge = bridge_by_call(db.node.as_ref(), parent_session_id, tool_call_id)
        .await
        .expect("accepted foreground bridge persists");
    assert_eq!(bridge.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(bridge.await_mode.as_deref(), Some("foreground"));
    assert_eq!(
        bridge.child_request_id.as_deref(),
        Some(child_request_id.as_str())
    );
    assert_eq!(
        child.caused_by_parent_tool_call_doc_id.as_deref(),
        bridge.doc_id.as_deref(),
        "child must be physically linked to the retained accepted bridge"
    );

    turn.backend.release(child_prompt);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let parent = request_by_id(db.node.as_ref(), parent_request_id).await;
            let child = request_by_id(db.node.as_ref(), &child_request_id).await;
            let bridge = bridge_by_call(db.node.as_ref(), parent_session_id, tool_call_id).await;
            if parent.as_ref().and_then(|row| row.lifecycle_state)
                == Some(RequestLifecycleState::Completed)
                && child.as_ref().and_then(|row| row.lifecycle_state)
                    == Some(RequestLifecycleState::Completed)
                && bridge
                    .as_ref()
                    .and_then(|row| row.lifecycle_state.as_deref())
                    == Some("completed")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("child terminal must release capacity and resume exact parent completion");
    let result = gents::tool_call_lifecycle::load_tool_call_result(
        &gents::config_client::ConfigAccess::Local(db.node.clone()),
        bridge.doc_id.as_deref().expect("physical bridge document"),
        &agent_did,
        parent_session_id,
        Some(&agent_did),
    )
    .await
    .expect("load resumed foreground bridge result");
    let result: serde_json::Value = serde_json::from_str(
        &gents::tool_call_lifecycle::render_tool_result(&result)
            .expect("render canonical bridge result"),
    )
    .expect("foreground bridge returns a canonical result envelope");
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["child_request_id"], child_request_id);
    assert_eq!(result["behavior_id"], behavior_id);
    assert_eq!(result["final_response"], "child running");
    assert_eq!(
        turn.backend
            .observed_requests("same behavior parent capacity one"),
        2,
        "the retained parent must make its post-child provider continuation"
    );
    turn.shutdown().await;
}
