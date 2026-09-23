use std::time::Duration;

use gents::__test_internals::{handle_list_subagents, ListSubagentsArgs};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{default_behavior_id_for_agent, DocumentRuntimeOptions, ToolCeiling};
use gents_protocol::row::{AgentRequestRow, AgentToolCallRow};
use serde::Deserialize;

use crate::support::accepted_turn::{boot_accepted_turn, AcceptedTurnSpec};
use crate::support::fixtures::{configure_subagent_behavior, subagent_target};
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse, StreamScript};
use crate::support::{first_optional_row, first_row, test_db};

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

/// Resolve the physical document id of an accepted provider tool call from the
/// canonical assistant header that introduced it (the exemplar seam used by
/// r4_subagent_tools.rs).
async fn accepted_tool_call_doc_id(
    node: &EmbeddedNode,
    session_id: &str,
    provider_tool_call_id: &str,
) -> String {
    let escaped_session_id = escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, order: {{ sequence: ASC }}) {{ _docID agent_did requester_did }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "load accepted headers: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data["AgentMessage"].as_array())
        .expect("AgentMessage header rows");
    for row in rows {
        let header_doc_id = row["_docID"].as_str().expect("header _docID");
        let agent_did = row["agent_did"].as_str().expect("header agent_did");
        let requester_did = row["requester_did"].as_str();
        let (header, _) = gents::session::load_canonical_message_from_node(
            node,
            header_doc_id,
            agent_did,
            requester_did,
        )
        .await
        .expect("reconstruct accepted canonical header");
        for block in header.blocks {
            if let gents_protocol::output::MessageBlock::ToolCall {
                tool_call_doc_id,
                id,
                call_id,
                ..
            } = block
            {
                if id == provider_tool_call_id || call_id.as_deref() == Some(provider_tool_call_id)
                {
                    return tool_call_doc_id;
                }
            }
        }
    }
    panic!("accepted provider tool call {provider_tool_call_id} missing from session {session_id}")
}

/// Fetch one AgentToolCall bridge row by provider tool-call id, selecting the
/// fields the caller names.
async fn fetch_tool_call_row(
    node: &EmbeddedNode,
    session_id: &str,
    provider_tool_call_id: &str,
    fields: &str,
) -> AgentToolCallRow {
    let tool_call_doc_id = accepted_tool_call_doc_id(node, session_id, provider_tool_call_id).await;
    let escaped_tool_call_doc_id = escape_graphql_string(&tool_call_doc_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ _docID: {{ _eq: "{escaped_tool_call_doc_id}" }} }},
                limit: 1
            ) {{ tool_call_key {fields} }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
}

/// Observe a background child request by exact parent lineage and prompt;
/// the child request id is allocated by the runtime, so it is only ever read
/// from persisted rows, never invented by the test.
async fn wait_for_child_request_by_lineage_and_prompt(
    node: &EmbeddedNode,
    parent_request_id: &str,
    prompt: &str,
) -> AgentRequestRow {
    let escaped_parent_request_id = escape_graphql_string(parent_request_id);
    let escaped_prompt = escape_graphql_string(prompt);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    caused_by_parent_request_id: {{ _eq: "{escaped_parent_request_id}" }},
                    content: {{ _eq: "{escaped_prompt}" }}
                }},
                limit: 1
            ) {{
                request_id
                session_id
                behavior_id
                content
                lifecycle_state
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
                caused_by_trigger_id
                caused_by_trigger_kind
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
            "timed out waiting for background child of {parent_request_id} with prompt {prompt:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn local_background_spawn_materializes_child_with_lineage_and_lists() {
    let db = test_db("convergence-local-background").await;
    let agent_did = db.node_identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&agent_did);

    // The accepted-turn public runtime owns the subagent source itself; the
    // parent self-spawns the same behavior as the legacy direct-dispatch test.
    let child_behavior_id = behavior_id.clone();
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        "convergence-bg-spawn-tools",
        vec![subagent_target(
            &agent_did,
            child_behavior_id.clone(),
            agent_did.clone(),
            child_behavior_id.clone(),
        )],
        true,
        true,
        None,
    )
    .await;
    let provider_call_id = "convergence-bg-tc";
    let parent_request_id = "convergence-bg-parent";
    let parent_session_id = "convergence-bg-session";
    let args = serde_json::json!({
        "name": child_behavior_id,
        "prompt": "background child work",
        "await_mode": "background"
    })
    .to_string();

    // The parent turn scripts the spawn tool call and then pauses so the
    // background child stays running while the assertions observe rows.
    let turn = boot_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "backend-convergence-bg",
            model: "model-convergence-bg",
            parent_behavior_id: &behavior_id,
            configured_behavior_ids: &[&behavior_id],
            request_id: parent_request_id,
            session_id: parent_session_id,
            prompt: "parent prompt background",
            accepted_chunks: vec![StreamChunk::tool_call(
                provider_call_id,
                "spawn_subagent",
                &args,
            )],
            child_plans: vec![StreamPlan::new(
                "background child work",
                vec![StreamResponse::Stream(StreamScript::paused(
                    "background child work",
                    ["child started"],
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

    // Discover the child request by exact parent lineage and prompt; the
    // provider tool-call id is a logical provider-layer id and must never be
    // read as a physical document id.
    let child = wait_for_child_request_by_lineage_and_prompt(
        db.node.as_ref(),
        parent_request_id,
        "background child work",
    )
    .await;
    let child_request_id = child.request_id.clone();
    assert!(
        !child_request_id.is_empty(),
        "child AgentRequest must carry an observed request id"
    );
    assert_eq!(
        child.behavior_id.as_deref(),
        Some(child_behavior_id.as_str())
    );
    assert_eq!(child.content.as_deref(), Some("background child work"));
    assert_eq!(child.subagent_depth, Some(1));
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(parent_request_id)
    );

    // The bridge: the accepted spawn admission resolves the parent tool call
    // to its physical document id, which the child's logical linkage carries.
    let parent_tool_call_doc_id =
        accepted_tool_call_doc_id(db.node.as_ref(), parent_session_id, provider_call_id).await;
    let parent_tool_call_id = fetch_tool_call_row(
        db.node.as_ref(),
        parent_session_id,
        provider_call_id,
        "tool_call_id child_request_id lifecycle_state await_mode",
    )
    .await
    .tool_call_id
    .expect("bridge tool call must retain its runtime identity");
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        Some(parent_tool_call_id.as_str()),
        "child logical linkage must name the bridge's runtime identity"
    );
    assert_eq!(
        child.caused_by_trigger_id.as_deref(),
        Some(parent_tool_call_id.as_str())
    );
    assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("subagent"));
    assert_ne!(
        parent_tool_call_id, parent_tool_call_doc_id,
        "provider id and physical bridge doc id are distinct layers"
    );
    let child_linkage_doc_id = child
        .caused_by_parent_tool_call_doc_id
        .as_deref()
        .expect("child AgentRequest must persist the physical bridge reference");
    assert_eq!(
        child_linkage_doc_id, parent_tool_call_doc_id,
        "child lineage must carry the bridge's physical doc id, not the provider id"
    );
    assert_ne!(
        child_linkage_doc_id, parent_tool_call_id,
        "the provider id must never appear where a physical doc id is expected"
    );

    let bridge = fetch_tool_call_row(
        db.node.as_ref(),
        parent_session_id,
        provider_call_id,
        "child_request_id lifecycle_state await_mode cancel_policy",
    )
    .await;
    assert_eq!(
        bridge.child_request_id.as_deref(),
        Some(child_request_id.as_str()),
        "the accepted spawn admission must bind the bridge to the observed child"
    );
    assert_eq!(bridge.await_mode.as_deref(), Some("background"));

    let resp = handle_list_subagents(&db.node, parent_request_id, ListSubagentsArgs::default())
        .await
        .expect("handle_list_subagents must not error");
    let entry = resp
        .entries
        .iter()
        .find(|e| e.child_request_id == child_request_id)
        .expect("list_subagents must reflect the running background child");
    assert_eq!(
        entry.behavior_id.as_deref(),
        Some(child_behavior_id.as_str())
    );

    turn.shutdown().await;
}

#[tokio::test]
async fn local_foreground_spawn_materializes_child_via_source() {
    let db = test_db("convergence-local-foreground").await;
    let agent_did = db.node_identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&agent_did);

    // The accepted-turn public runtime owns the subagent source. The child
    // must be a distinct behavior: a foreground wait blocks the parent
    // behavior's single executor slot (the per-behavior daemon processes one
    // request to completion), so only a separate behavior executor can claim
    // the child request while the parent is still running.
    let child_behavior_id = "convergence-fg-child-behavior".to_string();
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        &child_behavior_id,
        "convergence-fg-child-tools",
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        "convergence-fg-spawn-tools",
        vec![subagent_target(
            &agent_did,
            child_behavior_id.clone(),
            agent_did.clone(),
            child_behavior_id.clone(),
        )],
        true,
        true,
        None,
    )
    .await;

    let provider_call_id = "convergence-fg-tc";
    let parent_request_id = "convergence-fg-parent";
    let parent_session_id = "convergence-fg-session";
    let args = serde_json::json!({
        "name": child_behavior_id,
        "prompt": "foreground child work",
        "deadline": (chrono::Utc::now() + chrono::Duration::minutes(5))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    })
    .to_string();

    // The parent turn scripts the foreground spawn tool call; the child plan
    // is paused before EOF so the test can observe the real running wait,
    // then release it and observe the exact terminal result.
    let turn = boot_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "backend-convergence-fg",
            model: "model-convergence-fg",
            parent_behavior_id: &behavior_id,
            configured_behavior_ids: &[&behavior_id, &child_behavior_id],
            request_id: parent_request_id,
            session_id: parent_session_id,
            prompt: "parent prompt foreground",
            accepted_chunks: vec![StreamChunk::tool_call(
                provider_call_id,
                "spawn_subagent",
                &args,
            )],
            child_plans: vec![StreamPlan::new(
                "foreground child work",
                vec![StreamResponse::Stream(StreamScript::paused(
                    "foreground child work",
                    ["child started"],
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

    // Discover the child request by exact parent lineage and prompt; the child
    // request id is allocated by the runtime, so it is only ever read from
    // persisted rows, never invented by the test.
    let child = wait_for_child_request_by_lineage_and_prompt(
        db.node.as_ref(),
        parent_request_id,
        "foreground child work",
    )
    .await;
    let child_request_id = child.request_id.clone();
    assert!(
        !child_request_id.is_empty(),
        "child AgentRequest must carry an observed request id"
    );
    assert_eq!(
        child.behavior_id.as_deref(),
        Some(child_behavior_id.as_str())
    );
    assert_eq!(child.content.as_deref(), Some("foreground child work"));
    assert_eq!(child.subagent_depth, Some(1));
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(parent_request_id)
    );

    // Real running wait: while the child provider plan is paused before EOF,
    // both the child request and the bridge must be running — the foreground
    // wait is live, not fabricated by a direct hook.
    assert_child_request_running(db.node.as_ref(), &child_request_id).await;
    assert_eq!(
        fetch_tool_call_state(db.node.as_ref(), parent_session_id, provider_call_id)
            .await
            .expect("bridge tool call row must exist while the child runs"),
        "running"
    );

    // Release the paused child stream and let the runtime carry the terminal.
    turn.backend.release("foreground child work");

    // The bridge: the accepted spawn admission resolves the parent tool call
    // to its physical document id, which the child's logical linkage carries.
    let parent_tool_call_doc_id =
        accepted_tool_call_doc_id(db.node.as_ref(), parent_session_id, provider_call_id).await;
    let parent_tool_call_id = fetch_tool_call_row(
        db.node.as_ref(),
        parent_session_id,
        provider_call_id,
        "tool_call_id child_request_id lifecycle_state await_mode",
    )
    .await
    .tool_call_id
    .expect("bridge tool call must retain its runtime identity");
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        Some(parent_tool_call_id.as_str()),
        "child logical linkage must name the bridge's runtime identity"
    );
    assert_eq!(
        child.caused_by_trigger_id.as_deref(),
        Some(parent_tool_call_id.as_str())
    );
    assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("subagent"));
    assert_ne!(
        parent_tool_call_id, parent_tool_call_doc_id,
        "provider id and physical bridge doc id are distinct layers"
    );
    let child_linkage_doc_id = child
        .caused_by_parent_tool_call_doc_id
        .as_deref()
        .expect("child AgentRequest must persist the physical bridge reference");
    assert_eq!(
        child_linkage_doc_id, parent_tool_call_doc_id,
        "child lineage must carry the bridge's physical doc id, not the provider id"
    );
    assert_ne!(
        child_linkage_doc_id, parent_tool_call_id,
        "the provider id must never appear where a physical doc id is expected"
    );

    let bridge = fetch_tool_call_row(
        db.node.as_ref(),
        parent_session_id,
        provider_call_id,
        "child_request_id lifecycle_state await_mode cancel_policy",
    )
    .await;
    assert_eq!(
        bridge.child_request_id.as_deref(),
        Some(child_request_id.as_str()),
        "the accepted spawn admission must bind the bridge to the observed child"
    );
    assert_eq!(bridge.await_mode.as_deref(), Some("foreground"));

    // The accepted foreground wait completes only after the child terminal;
    // the result is read through the canonical result owner, never a row
    // payload column.
    wait_for_bridge_state(
        db.node.as_ref(),
        parent_session_id,
        provider_call_id,
        "completed",
    )
    .await;
    let result_message = gents::tool_call_lifecycle::load_tool_call_result(
        &gents::config_client::ConfigAccess::Local(db.node.clone()),
        &parent_tool_call_doc_id,
        &agent_did,
        parent_session_id,
        Some(&agent_did),
    )
    .await
    .expect("load canonical foreground bridge reply");
    let result: serde_json::Value = serde_json::from_str(
        &gents::tool_call_lifecycle::render_tool_result(&result_message)
            .expect("render canonical foreground bridge reply"),
    )
    .expect("foreground bridge returns a canonical result envelope");
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["child_request_id"], child_request_id);
    assert_eq!(result["behavior_id"], child_behavior_id);
    assert_eq!(result["final_response"], "child started");

    turn.shutdown().await;
}

// Real running-wait helpers for the foreground scenario: both observations
// poll persisted rows, never a test-side lifecycle machine.
async fn assert_child_request_running(node: &EmbeddedNode, child_request_id: &str) {
    let escaped = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{ request_id lifecycle_state }}
        }}"#
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let state =
            first_optional_row::<AgentRequestRow>(&node.execute(&query).await, "AgentRequest")
                .and_then(|row| row.lifecycle_state);
        if state == Some(gents_protocol::request_lifecycle::RequestLifecycleState::Processing) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for child request {child_request_id} to reach processing (saw {state:?})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_bridge_state(
    node: &EmbeddedNode,
    session_id: &str,
    provider_call_id: &str,
    expected: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let state = fetch_tool_call_state(node, session_id, provider_call_id)
            .await
            .unwrap_or_default();
        if state == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for bridge {provider_call_id} to reach {expected} (saw {state})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
