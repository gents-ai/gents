use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::{
    create_subagent_request_with_request_id,
    create_subagent_request_with_trusted_parent_request_id, IllegalToolCallTransition,
    ToolCallLifecycle, MAX_SUBAGENT_DEPTH,
};
use gents::{
    default_behavior_id_for_agent, AgentIdentity, DocumentRuntimeOptions, Gents, ToolCeiling,
};
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::support::accepted_turn::{boot_accepted_turn, AcceptedTurnSpec};
use crate::support::fixtures::{
    bind_default_behavior_backend, configure_subagent_behavior, subagent_target, test_identity,
};
use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::{fetch_tool_call_snapshots_for_session, ToolCallSnapshot};
use crate::support::streaming_backend::StreamChunk;
use crate::support::{first_optional_row, first_row, test_db};

struct RunningAgent {
    booted: BootedAgent,
    _endpoint: MockModelEndpoint,
    behavior_id: String,
}

async fn boot_agent(db: &crate::support::TestDb, test_name: &str) -> RunningAgent {
    boot_agent_with_policy(db, test_name, None, true, true).await
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
    assert!(
        agent.unavailable_behaviors().is_empty(),
        "subagent fixture produced unavailable behaviors: {:?}",
        agent.unavailable_behaviors()
    );
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

/// Single fixture owner for a behavior's canonical subagent authorization.
async fn ensure_subagent_authorization(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    selection_id: &str,
    subagent_targets: Vec<(String, String, String)>,
    spawn_enabled: bool,
    background_enabled: bool,
    allow_cross_principal: Option<bool>,
) {
    let target_entries = subagent_targets
        .into_iter()
        .map(|(target_name, target_did, target_behavior)| {
            subagent_target(agent_did, target_name, target_did, target_behavior)
        })
        .collect();
    configure_subagent_behavior(
        node,
        agent_did,
        behavior_id,
        selection_id,
        target_entries,
        spawn_enabled,
        background_enabled,
        allow_cross_principal,
    )
    .await;
}

async fn ensure_parent_subagent_authorization(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    subagent_targets: Vec<String>,
    spawn_enabled: bool,
    background_enabled: bool,
) {
    let subagent_targets = subagent_targets
        .into_iter()
        .map(|target_behavior_id| {
            (
                target_behavior_id.clone(),
                agent_did.to_string(),
                target_behavior_id,
            )
        })
        .collect();
    ensure_subagent_authorization(
        node,
        agent_did,
        behavior_id,
        &format!("{behavior_id}-r3-subagent-tools"),
        subagent_targets,
        spawn_enabled,
        background_enabled,
        None,
    )
    .await;
}

async fn wait_for_child_request(node: &EmbeddedNode, child_request_id: &str) -> AgentRequestRow {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{
                request_id
                agent_did
                requester_did
                behavior_id
                content
                lifecycle_state
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
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
        if tokio::time::Instant::now() >= deadline {
            let evidence = node
                .execute(
                    r#"{ AgentToolCall { _docID tool_call_key request_id request_doc_id agent_did tool_call_id tool_name lifecycle_state tool_failure_class denial_reason child_request_id spawn_target_did await_mode cancel_policy } }"#,
                )
                .await;
            panic!(
                "timed out waiting for child AgentRequest {child_request_id}; tool calls={:?}; errors={:?}",
                evidence.data, evidence.errors
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn trusted_cross_principal_path_uses_targeted_bridge_without_parent_replication() {
    let db = test_db("r3-subagent-source-xdep-targeted-bridge").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("r3-xdep-targeted-bridge"));
    let local_did = identity.did().to_string();
    let coordinator_did = "did:key:zTargetedCoordinator";
    let target_behavior_id = "xdep-target-targeted-bridge";
    let parent_request_id = "r3-parent-targeted-bridge-not-replicated";
    let parent_tool_call_id = "r3-tc-targeted-bridge";
    let child_request_id = "r3-child-targeted-bridge";

    upsert_target_behavior_with_cross_principal(
        db.node.as_ref(),
        &local_did,
        target_behavior_id,
        true,
    )
    .await;

    let mut paired = std::collections::HashSet::new();
    paired.insert(coordinator_did.to_string());
    let _source = crate::support::fixtures::spawn_subagent_source_with_authorized_peers(
        db.node.clone(),
        &local_did,
        target_behavior_id,
        target_behavior_id,
        paired,
    );
    wait_for_subagent_source_subscription().await;

    write_targeted_cross_principal_bridge(
        db.node.as_ref(),
        coordinator_did,
        parent_request_id,
        parent_tool_call_id,
        child_request_id,
        target_behavior_id,
        &local_did,
        1,
    )
    .await;

    let child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_eq!(child.agent_did.as_deref(), Some(local_did.as_str()));
    assert_eq!(child.requester_did.as_deref(), Some(coordinator_did));
    assert_eq!(child.subagent_depth, Some(2));
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(parent_request_id)
    );
    assert_eq!(
        child.caused_by_parent_request_doc_id.as_deref(),
        Some(format!("remote-doc-{parent_request_id}").as_str())
    );
    assert!(child.caused_by_parent_tool_call_doc_id.is_some());
}

#[tokio::test]
async fn imported_over_depth_bridge_cannot_materialize_a_trusted_remote_child() {
    use crate::support::r5_conformance::scenario::{ModeledAction, ModeledScenario};

    let modeled: ModeledScenario = serde_json::from_value(
        crate::lean_vocab_test::lean_r5_scenario_cases()
            .iter()
            .find(|case| case["name"] == "remote_depth_ceiling")
            .expect("Lean R5 depth case")
            .clone(),
    )
    .expect("decode generated depth case");
    let depths = modeled.actions.iter().filter_map(|action| match action {
        ModeledAction::RejectSpawnInvocation { parent_depth, .. } => Some(*parent_depth),
        _ => None,
    });
    let [parent_depth] = depths
        .collect::<Vec<_>>()
        .try_into()
        .unwrap_or_else(|rows: Vec<_>| {
            panic!("Lean R5 depth case must contain one rejected invocation, got {rows:?}")
        });
    assert_eq!(parent_depth, MAX_SUBAGENT_DEPTH);

    let db = test_db("r5-imported-over-depth-bridge").await;
    let local_did = db.node_identity.did().to_string();
    let coordinator_did = "did:key:zR5OverDepthCoordinator";
    let target_behavior = "r5-over-depth-target";
    let parent_request = "r5-over-depth-parent";
    let parent_tool = "r5-over-depth-tool";
    let child_request = "r5-over-depth-child";
    upsert_target_behavior_with_cross_principal(
        db.node.as_ref(),
        &local_did,
        target_behavior,
        true,
    )
    .await;
    let mut paired = std::collections::HashSet::new();
    paired.insert(coordinator_did.to_string());
    let _source = crate::support::fixtures::spawn_subagent_source_with_authorized_peers(
        db.node.clone(),
        &local_did,
        target_behavior,
        target_behavior,
        paired,
    );
    wait_for_subagent_source_subscription().await;

    // A foreign bridge is an input to the host scan, not a claim that the
    // coordinator accepted or published a valid provider turn at this depth.
    let (parent_doc, tool_doc) = write_targeted_cross_principal_bridge(
        db.node.as_ref(),
        coordinator_did,
        parent_request,
        parent_tool,
        child_request,
        target_behavior,
        &local_did,
        parent_depth,
    )
    .await;
    let error = create_subagent_request_with_trusted_parent_request_id(
        db.node.as_ref(),
        child_request.to_string(),
        parent_request.to_string(),
        parent_doc,
        parent_tool.to_string(),
        tool_doc,
        parent_depth,
        local_did,
        target_behavior.to_string(),
        "targeted cross-principal child prompt".to_string(),
        None,
        coordinator_did.to_string(),
    )
    .await
    .expect_err("over-depth imported bridge must fail the real trusted child owner");
    assert!(
        matches!(
            error.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::SubagentDepthExceeded)
        ),
        "trusted remote child owner returned an unrelated error: {error:#}"
    );
    assert_no_child_request_for_tool(db.node.as_ref(), parent_tool, Duration::from_millis(750))
        .await;
}

#[tokio::test]
async fn trusted_path_rejects_noncanonical_bridge_author() {
    let db = test_db("r3-subagent-source-xdep-requester-normalization").await;
    let agent_did = db.node_identity.did().to_string();
    let child_request_id = "r3-child-normalized-requester";
    let (parent_request_doc_id, parent_tool_call_doc_id) = write_targeted_cross_principal_bridge(
        db.node.as_ref(),
        "did:key:zNormalizedRequester",
        "r3-parent-not-replicated",
        "r3-tc-normalized-requester",
        child_request_id,
        "test",
        &agent_did,
        0,
    )
    .await;

    let error = create_subagent_request_with_trusted_parent_request_id(
        db.node.as_ref(),
        child_request_id.to_string(),
        "r3-parent-not-replicated".to_string(),
        parent_request_doc_id,
        "r3-tc-normalized-requester".to_string(),
        parent_tool_call_doc_id,
        0,
        agent_did.clone(),
        "test".to_string(),
        "prompt".to_string(),
        None,
        "  did:key:zNormalizedRequester  ".to_string(),
    )
    .await
    .expect_err("signed bridge author identifiers must be canonical before authoring");
    assert!(
        matches!(
            error.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::ParentLinkageIncoherent)
        ),
        "expected ParentLinkageIncoherent, got {error:?}"
    );
    assert!(error
        .to_string()
        .contains("AgentRequest parent linkage incoherent"));
    assert_no_child_request_for_tool(
        db.node.as_ref(),
        "r3-tc-normalized-requester",
        Duration::ZERO,
    )
    .await;
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

/// Fetch one tool call through the shared snapshot owner instead of a local
/// projection; the fixture rows carry every snapshot field.
async fn fetch_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> ToolCallSnapshot {
    fetch_tool_call_snapshots_for_session(node, session_id)
        .await
        .into_iter()
        .find(|snapshot| snapshot.tool_call_id == tool_call_id)
        .unwrap_or_else(|| panic!("AgentToolCall {session_id}/{tool_call_id} must exist"))
}

async fn wait_for_tool_call_terminal(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_doc_id: &str,
) -> ToolCallSnapshot {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(tool) = fetch_tool_call_snapshots_for_session(node, session_id)
            .await
            .into_iter()
            .find(|snapshot| snapshot.doc_id == tool_call_doc_id)
        {
            if tool.lifecycle_state.as_deref() == Some("failed") {
                return tool;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "accepted tool {session_id}/{tool_call_doc_id} did not reach failed lifecycle"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn accepted_denied_spawn(
    db: &crate::support::TestDb,
    request_id: &str,
    session_id: &str,
    tool_call_id: &str,
    target_name: &str,
    configured_targets: Vec<String>,
    spawn_enabled: bool,
    background_enabled: bool,
    background: bool,
) -> ToolCallSnapshot {
    let agent_did = db.node_identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&agent_did);
    ensure_parent_subagent_authorization(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        configured_targets,
        spawn_enabled,
        background_enabled,
    )
    .await;
    let mut args = serde_json::json!({
        "name": target_name,
        "prompt": "denied child prompt",
    });
    if background {
        args["await_mode"] = serde_json::json!("background");
    }
    let turn = boot_accepted_turn(
        db,
        AcceptedTurnSpec {
            backend_id: "backend-subagent-source-denial",
            model: "model-subagent-source-denial",
            parent_behavior_id: &behavior_id,
            configured_behavior_ids: &[&behavior_id],
            request_id,
            session_id,
            prompt: "parent prompt",
            accepted_chunks: vec![StreamChunk::tool_call(
                tool_call_id,
                "spawn_subagent",
                &args.to_string(),
            )],
            child_plans: Vec::new(),
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
    let tool_doc_id = accepted_tool_call_doc_id(db.node.as_ref(), session_id, tool_call_id).await;
    let tool = wait_for_tool_call_terminal(db.node.as_ref(), session_id, &tool_doc_id).await;
    assert_no_child_request_for_tool(
        db.node.as_ref(),
        &tool.tool_call_id,
        Duration::from_millis(750),
    )
    .await;
    turn.shutdown().await;
    tool
}

async fn assert_tool_call_not_allowed(
    node: &Arc<EmbeddedNode>,
    tool: &ToolCallSnapshot,
    expected_path: &str,
    expected_requested: &str,
) {
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable")
    );
    let result: serde_json::Value =
        serde_json::from_str(&tool.load_result(node.clone()).await).unwrap();
    assert_eq!(result["failure_class"], "tool_not_allowed");
    assert_eq!(result["service_id"], "subagent");
    assert_eq!(result["path"], expected_path);
    assert_eq!(result["requested_tool_name"], expected_requested);
}

#[tokio::test]
async fn subagent_source_materializes_child_request_from_tool_call() {
    let db = test_db("r3-subagent-source-spawn").await;
    let agent_did = db.node_identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&agent_did);
    let parent_request_id = "r3-parent-spawn";
    let parent_session_id = "r3-session-spawn";
    let parent_tool_call_id = "r3-tc-spawn";
    let child_prompt = "child prompt from source";

    // The parent's subagent authorization fixture (self target, spawn and
    // background enabled) is written before the runtime boots; the accepted
    // turn owns all publication below.
    ensure_parent_subagent_authorization(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        vec![behavior_id.clone()],
        true,
        true,
    )
    .await;

    // The accepted provider turn owns publication; the model args name the
    // self target already configured by the boot fixture, never an invented
    // accepted header. Background mode keeps the child running for the
    // physical-binding assertions below.
    let args = serde_json::json!({
        "name": behavior_id.clone(),
        "prompt": child_prompt,
        "await_mode": "background"
    })
    .to_string();
    let turn = boot_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "backend-subagent-source",
            model: "model-subagent-source-spawn",
            parent_behavior_id: &behavior_id,
            configured_behavior_ids: &[&behavior_id],
            request_id: parent_request_id,
            session_id: parent_session_id,
            prompt: "parent prompt",
            accepted_chunks: vec![StreamChunk::tool_call(
                parent_tool_call_id,
                "spawn_subagent",
                &args,
            )],
            child_plans: Vec::new(),
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

    // The child request id is allocated by the runtime; discover the child by
    // exact parent lineage and prompt instead of inventing an id.
    let child = wait_for_child_request_by_lineage_and_prompt(
        db.node.as_ref(),
        parent_request_id,
        child_prompt,
    )
    .await;
    let observed_child_request_id = child.request_id.clone();
    assert!(
        !observed_child_request_id.is_empty(),
        "child AgentRequest must carry an observed request id"
    );
    assert_eq!(child.agent_did.as_deref(), Some(agent_did.as_str()));
    assert_eq!(child.behavior_id.as_deref(), Some(behavior_id.as_str()));
    assert_eq!(child.content.as_deref(), Some(child_prompt));
    assert_eq!(child.subagent_depth, Some(1));
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(parent_request_id)
    );

    // The bridge row: the accepted spawn admission must bind the accepted
    // tool call to the observed child request id. The bridge is read as the
    // canonical AgentToolCall row projection; the snapshot owner does not
    // carry the spawn-admission fields.
    let bridge_query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ _docID: {{ _eq: "{}" }} }},
                limit: 1
            ) {{
                _docID tool_call_id child_request_id await_mode
            }}
        }}"#,
        escape_graphql_string(
            &accepted_tool_call_doc_id(db.node.as_ref(), parent_session_id, parent_tool_call_id,)
                .await
        )
    );
    let bridge_response = db.node.execute(&bridge_query).await;
    assert!(
        !bridge_response.has_errors(),
        "bridge row query failed: {:?}",
        bridge_response.errors
    );
    #[derive(Deserialize)]
    struct AcceptedBridgeIdentity {
        #[serde(rename = "_docID")]
        doc_id: String,
        tool_call_id: String,
        child_request_id: Option<String>,
        await_mode: Option<String>,
    }
    let bridge = first_row::<AcceptedBridgeIdentity>(&bridge_response, "AgentToolCall");
    let bridge_doc_id = bridge.doc_id;
    let child_request_id = observed_child_request_id;
    assert_eq!(
        bridge.child_request_id.as_deref(),
        Some(child_request_id.as_str()),
        "accepted spawn admission must bind the tool call to the observed child"
    );
    assert_eq!(bridge.await_mode.as_deref(), Some("background"));
    let parent_tool_call_doc_id = bridge_doc_id;
    let bridge_tool_call_id = bridge.tool_call_id;
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        Some(bridge_tool_call_id.as_str()),
        "child logical linkage must name the bridge's runtime identity"
    );
    assert_eq!(
        child.caused_by_trigger_id.as_deref(),
        Some(bridge_tool_call_id.as_str())
    );
    assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("subagent"));
    assert_ne!(
        bridge_tool_call_id, parent_tool_call_doc_id,
        "provider id and physical bridge doc id are distinct layers"
    );

    turn.shutdown().await;
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
                agent_did
                requester_did
                behavior_id
                content
                lifecycle_state
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
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
            "timed out waiting for child of {parent_request_id} with prompt {prompt:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Resolve the physical document id of an accepted provider tool call from the
/// canonical assistant header that introduced it. This is the exemplar seam
/// pattern used by subagent_convergence.rs; the provider id is a logical id
/// and must never be read as a physical document id.
async fn accepted_tool_call_doc_id(
    node: &EmbeddedNode,
    session_id: &str,
    provider_tool_call_id: &str,
) -> String {
    let escaped_session_id = escape_graphql_string(session_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
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
                    if id == provider_tool_call_id
                        || call_id.as_deref() == Some(provider_tool_call_id)
                    {
                        return tool_call_doc_id;
                    }
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("accepted provider tool call {provider_tool_call_id} missing from session {session_id}")
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn subagent_source_rejects_unauthorized_target_without_child_request() {
    let db = test_db("r3-subagent-source-unauthorized").await;
    let parent_request_id = "r3-parent-unauthorized";
    let parent_tool_call_id = "r3-tc-unauthorized";
    let parent_session_id = "r3-session-unauthorized";
    let behavior_id = default_behavior_id_for_agent(db.node_identity.did());
    let tool = accepted_denied_spawn(
        &db,
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        &behavior_id,
        Vec::new(),
        true,
        true,
        false,
    )
    .await;
    assert_tool_call_not_allowed(&db.node, &tool, "/name", &behavior_id).await;
}

#[tokio::test]
async fn subagent_source_fails_unauthorized_target_even_when_target_is_not_active() {
    let db = test_db("r3-subagent-source-unauthorized-inactive").await;
    let parent_request_id = "r3-parent-unauthorized-inactive";
    let parent_tool_call_id = "r3-tc-unauthorized-inactive";
    let parent_session_id = "r3-session-unauthorized-inactive";
    let target_behavior_id = "not-active-or-authorized";
    let tool = accepted_denied_spawn(
        &db,
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        target_behavior_id,
        Vec::new(),
        true,
        true,
        false,
    )
    .await;
    assert_tool_call_not_allowed(&db.node, &tool, "/name", target_behavior_id).await;
}

#[tokio::test]
async fn subagent_source_rejects_when_spawn_disabled_even_with_authorized_target() {
    let db = test_db("r3-subagent-source-spawn-disabled").await;
    let behavior_id = default_behavior_id_for_agent(db.node_identity.did());
    let parent_request_id = "r3-parent-spawn-disabled";
    let parent_tool_call_id = "r3-tc-spawn-disabled";
    let parent_session_id = "r3-session-spawn-disabled";
    let tool = accepted_denied_spawn(
        &db,
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        &behavior_id,
        vec![behavior_id.clone()],
        false,
        true,
        false,
    )
    .await;
    assert_tool_call_not_allowed(&db.node, &tool, "/", "spawn_subagent").await;
}

#[tokio::test]
async fn subagent_source_rejects_background_when_background_disabled() {
    let db = test_db("r3-subagent-source-background-disabled").await;
    let behavior_id = default_behavior_id_for_agent(db.node_identity.did());
    let parent_request_id = "r3-parent-background-disabled";
    let parent_tool_call_id = "r3-tc-background-disabled";
    let parent_session_id = "r3-session-background-disabled";
    let tool = accepted_denied_spawn(
        &db,
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        &behavior_id,
        vec![behavior_id.clone()],
        true,
        false,
        true,
    )
    .await;
    assert_tool_call_not_allowed(&db.node, &tool, "/await_mode", "background").await;
}

#[tokio::test]
async fn subagent_source_ignores_native_tool_call_without_child_request_id() {
    let db = test_db("r3-subagent-source-native").await;
    let agent_did = db.node_identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&agent_did);
    let parent_request_id = "r3-parent-native";
    let parent_tool_call_id = "r3-tc-native";
    let turn = boot_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "backend-subagent-source-native",
            model: "model-subagent-source-native",
            parent_behavior_id: &behavior_id,
            configured_behavior_ids: &[&behavior_id],
            request_id: parent_request_id,
            session_id: "r3-session-native",
            prompt: "parent prompt",
            accepted_chunks: vec![StreamChunk::tool_call(
                parent_tool_call_id,
                "read_file",
                "{}",
            )],
            child_plans: Vec::new(),
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
    let tool_doc_id =
        accepted_tool_call_doc_id(db.node.as_ref(), "r3-session-native", parent_tool_call_id).await;
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ child_request_id }} }}"#,
        escape_graphql_string(&tool_doc_id)
    );
    let response = db.node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "read native tool bridge: {:?}",
        response.errors
    );
    assert!(
        response
            .data
            .as_ref()
            .and_then(|data| data["AgentToolCall"][0]["child_request_id"].as_str())
            .is_none(),
        "ordinary accepted native tool must not carry a child request id"
    );

    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(750),
    )
    .await;

    turn.shutdown().await;
}

#[tokio::test]
async fn create_subagent_request_rejects_nonexistent_parent_request() {
    let db = test_db("r3-subagent-source-missing-parent").await;
    let error = create_subagent_request_with_request_id(
        db.node.as_ref(),
        "r3-child-missing-parent".to_string(),
        "missing-parent-request".to_string(),
        "missing-parent-request-doc".to_string(),
        "r3-tc-missing-parent".to_string(),
        "missing-parent-tool-call-doc".to_string(),
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
async fn create_subagent_request_rejects_tool_call_from_another_parent_document() {
    let db = test_db("r3-subagent-source-mismatched-tool-parent").await;
    let first_parent_doc_id = crate::support::create_request(
        db.node.as_ref(),
        "r3-parent-first",
        "r3-parent-first-session",
        "processing",
        &chrono::Utc::now().to_rfc3339(),
    )
    .await;
    let second_parent_doc_id = crate::support::create_request(
        db.node.as_ref(),
        "r3-parent-second",
        "r3-parent-second-session",
        "processing",
        &chrono::Utc::now().to_rfc3339(),
    )
    .await;
    // An imported bridge can be malformed without claiming that a provider
    // accepted it. Exercise the child-admission owner's exact physical
    // linkage check against that untrusted row.
    write_cross_principal_bridge(
        db.node.as_ref(),
        crate::support::AGENT_DID,
        "r3-parent-first",
        &first_parent_doc_id,
        "r3-parent-first-session",
        "r3-parent-mismatch-tool",
        "r3-child-mismatched-tool-parent",
        "test",
        crate::support::AGENT_DID,
    )
    .await;
    let bridge = fetch_tool_call(
        db.node.as_ref(),
        "r3-parent-first-session",
        "r3-parent-mismatch-tool",
    )
    .await;

    let error = create_subagent_request_with_request_id(
        db.node.as_ref(),
        "r3-child-mismatched-tool-parent".to_string(),
        "r3-parent-second".to_string(),
        second_parent_doc_id,
        "r3-parent-mismatch-tool".to_string(),
        bridge.doc_id,
        0,
        crate::support::AGENT_DID.to_string(),
        "test".to_string(),
        "prompt".to_string(),
        None,
    )
    .await
    .expect_err("bridge request_doc_id must agree with the exact parent document");
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
    // The generated R5 depth scenario constructs a legal physical chain
    // through MAX_SUBAGENT_DEPTH; this direct owner test retains the two
    // overflow-safe rejections that fire before any document lookup.
    let error = create_subagent_request_with_request_id(
        db.node.as_ref(),
        "r3-child-depth-over".to_string(),
        "r3-parent-depth".to_string(),
        "r3-parent-depth-doc".to_string(),
        "r3-tc-depth-over".to_string(),
        "r3-tc-depth-over-doc".to_string(),
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

    let overflow_error = create_subagent_request_with_request_id(
        db.node.as_ref(),
        "r3-child-depth-overflow".to_string(),
        "r3-parent-depth".to_string(),
        "r3-parent-depth-doc".to_string(),
        "r3-tc-depth-overflow".to_string(),
        "r3-tc-depth-overflow-doc".to_string(),
        u32::MAX,
        crate::support::AGENT_DID.to_string(),
        "test".to_string(),
        "prompt".to_string(),
        None,
    )
    .await
    .expect_err("u32::MAX parent depth must be rejected without overflow");
    assert!(
        matches!(
            overflow_error.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::SubagentDepthExceeded)
        ),
        "expected overflow-safe SubagentDepthExceeded, got {overflow_error:?}"
    );
}

#[tokio::test]
async fn subagent_source_skips_child_when_resolved_did_is_remote() {
    let db = test_db("r3-subagent-source-did-anchor").await;
    let agent_did = db.node_identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&agent_did);
    let remote_did = "did:key:zRemotePeerNotUs";
    ensure_subagent_authorization(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        &format!("{behavior_id}-r3-did-anchor-tools"),
        vec![(
            "remote-target".to_string(),
            remote_did.to_string(),
            "remote-behavior".to_string(),
        )],
        true,
        true,
        Some(true),
    )
    .await;

    let parent_request_id = "r3-parent-did-anchor";
    let parent_session_id = "r3-session-did-anchor";
    let parent_tool_call_id = "r3-tc-did-anchor";
    let args = serde_json::json!({
        "name": "remote-target",
        "prompt": "child prompt that should not run here",
        "await_mode": "background"
    })
    .to_string();
    let turn = boot_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "backend-did-anchor",
            model: "model-did-anchor",
            parent_behavior_id: &behavior_id,
            configured_behavior_ids: &[&behavior_id],
            request_id: parent_request_id,
            session_id: parent_session_id,
            prompt: "parent prompt",
            accepted_chunks: vec![StreamChunk::tool_call(
                parent_tool_call_id,
                "spawn_subagent",
                &args,
            )],
            child_plans: Vec::new(),
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
    let tool_doc_id =
        accepted_tool_call_doc_id(db.node.as_ref(), parent_session_id, parent_tool_call_id).await;
    let escaped_doc_id = escape_graphql_string(&tool_doc_id);
    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{ tool_call_id child_request_id spawn_target_did }} }}"#
        ))
        .await;
    #[derive(Deserialize)]
    struct RemoteBridge {
        tool_call_id: String,
        child_request_id: Option<String>,
        spawn_target_did: Option<String>,
    }
    let bridge = first_row::<RemoteBridge>(&response, "AgentToolCall");
    assert_eq!(bridge.spawn_target_did.as_deref(), Some(remote_did));
    assert!(
        bridge
            .child_request_id
            .as_deref()
            .is_some_and(|id| !id.is_empty()),
        "accepted remote spawn must bind an exact child identity"
    );
    assert_no_child_request_for_tool(
        db.node.as_ref(),
        &bridge.tool_call_id,
        Duration::from_millis(800),
    )
    .await;
    turn.shutdown().await;
}

#[tokio::test]
async fn subagent_source_refuses_cross_principal_child_when_target_flag_off() {
    let db = test_db("r3-subagent-source-xdep-flag-off").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("r3-xdep-flag-off"));
    let local_did = identity.did().to_string();
    let target_behavior_id = "xdep-target-flag-off";
    let remote_parent_did = "did:key:zPairedPeerParent";

    upsert_target_behavior_with_cross_principal(
        db.node.as_ref(),
        &local_did,
        target_behavior_id,
        false,
    )
    .await;

    let parent_request_id = "r3-parent-xdep-flag-off";
    let parent_session_id = "r3-session-xdep-flag-off";
    let parent_tool_call_id = "r3-tc-xdep-flag-off";
    let child_request_id = "r3-child-xdep-flag-off";
    let parent_request_doc_id = create_remote_parent_request(
        db.node.as_ref(),
        remote_parent_did,
        parent_request_id,
        parent_session_id,
    )
    .await;

    let mut paired = std::collections::HashSet::new();
    paired.insert(remote_parent_did.to_string());
    let _source = crate::support::fixtures::spawn_subagent_source_with_authorized_peers(
        db.node.clone(),
        &local_did,
        target_behavior_id,
        target_behavior_id,
        paired,
    );
    wait_for_subagent_source_subscription().await;

    write_cross_principal_bridge(
        db.node.as_ref(),
        remote_parent_did,
        parent_request_id,
        &parent_request_doc_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
        target_behavior_id,
        &local_did,
    )
    .await;

    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(800),
    )
    .await;
}

#[tokio::test]
async fn subagent_source_materializes_cross_principal_child_when_target_flag_on() {
    let db = test_db("r3-subagent-source-xdep-flag-on").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("r3-xdep-flag-on"));
    let local_did = identity.did().to_string();
    let target_behavior_id = "xdep-target-flag-on";
    let remote_parent_did = "did:key:zPairedPeerParentOn";

    upsert_target_behavior_with_cross_principal(
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
    let parent_request_doc_id = create_remote_parent_request(
        db.node.as_ref(),
        remote_parent_did,
        parent_request_id,
        parent_session_id,
    )
    .await;

    let mut paired = std::collections::HashSet::new();
    paired.insert(remote_parent_did.to_string());
    let _source = crate::support::fixtures::spawn_subagent_source_with_authorized_peers(
        db.node.clone(),
        &local_did,
        target_behavior_id,
        target_behavior_id,
        paired,
    );
    wait_for_subagent_source_subscription().await;

    write_cross_principal_bridge(
        db.node.as_ref(),
        remote_parent_did,
        parent_request_id,
        &parent_request_doc_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
        target_behavior_id,
        &local_did,
    )
    .await;

    let child = wait_for_child_request(db.node.as_ref(), child_request_id).await;
    assert_eq!(child.request_id, child_request_id);
    assert_eq!(child.agent_did.as_deref(), Some(local_did.as_str()));
    assert_eq!(child.requester_did.as_deref(), Some(remote_parent_did));
    assert_eq!(child.behavior_id.as_deref(), Some(target_behavior_id));
}

#[tokio::test]
async fn trusted_path_refuses_spawn_targeting_other_principal() {
    let db = test_db("r3-subagent-source-xdep-wrong-principal").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("r3-xdep-wrong-principal"));
    let local_did = identity.did().to_string();
    let other_principal_did = "did:key:zDifferentTrustedPrincipal";
    let target_behavior_id = "xdep-target-wrong-principal";
    let remote_parent_did = "did:key:zPairedPeerParentWrongPrincipal";

    upsert_target_behavior_with_cross_principal(
        db.node.as_ref(),
        &local_did,
        target_behavior_id,
        true,
    )
    .await;

    let parent_request_id = "r3-parent-xdep-wrong-principal";
    let parent_session_id = "r3-session-xdep-wrong-principal";
    let parent_tool_call_id = "r3-tc-xdep-wrong-principal";
    let child_request_id = "r3-child-xdep-wrong-principal";
    let parent_request_doc_id = create_remote_parent_request(
        db.node.as_ref(),
        remote_parent_did,
        parent_request_id,
        parent_session_id,
    )
    .await;

    let mut paired = std::collections::HashSet::new();
    paired.insert(remote_parent_did.to_string());
    let _source = crate::support::fixtures::spawn_subagent_source_with_authorized_peers(
        db.node.clone(),
        &local_did,
        target_behavior_id,
        target_behavior_id,
        paired,
    );
    wait_for_subagent_source_subscription().await;

    write_cross_principal_bridge(
        db.node.as_ref(),
        remote_parent_did,
        parent_request_id,
        &parent_request_doc_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
        target_behavior_id,
        other_principal_did,
    )
    .await;

    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(800),
    )
    .await;
}

#[tokio::test]
async fn trusted_path_refuses_missing_spawn_target_did() {
    let db = test_db("r3-subagent-source-xdep-missing-target").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("r3-xdep-missing-target"));
    let local_did = identity.did().to_string();
    let target_behavior_id = "xdep-target-missing-target";
    let remote_parent_did = "did:key:zPairedPeerParentMissingTarget";

    upsert_target_behavior_with_cross_principal(
        db.node.as_ref(),
        &local_did,
        target_behavior_id,
        true,
    )
    .await;

    let parent_request_id = "r3-parent-xdep-missing-target";
    let parent_session_id = "r3-session-xdep-missing-target";
    let parent_tool_call_id = "r3-tc-xdep-missing-target";
    let child_request_id = "r3-child-xdep-missing-target";
    let parent_request_doc_id = create_remote_parent_request(
        db.node.as_ref(),
        remote_parent_did,
        parent_request_id,
        parent_session_id,
    )
    .await;

    let mut paired = std::collections::HashSet::new();
    paired.insert(remote_parent_did.to_string());
    let _source = crate::support::fixtures::spawn_subagent_source_with_authorized_peers(
        db.node.clone(),
        &local_did,
        target_behavior_id,
        target_behavior_id,
        paired,
    );
    wait_for_subagent_source_subscription().await;

    write_cross_principal_bridge_with_spawn_target(
        db.node.as_ref(),
        remote_parent_did,
        parent_request_id,
        &parent_request_doc_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
        target_behavior_id,
        &local_did,
        None,
    )
    .await;

    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(800),
    )
    .await;
}

#[tokio::test]
async fn recovery_ignores_remote_parent_orphan_even_when_target_is_local() {
    let db = test_db("r3-subagent-source-orphan-remote-parent").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("r3-orphan-remote-parent"));
    let local_did = identity.did().to_string();
    let target_behavior_id = "orphan-remote-parent-target";
    let remote_parent_did = "did:key:zRemoteParentForRecovery";
    let parent_request_id = "r3-parent-orphan-remote-parent";
    let parent_session_id = "r3-session-orphan-remote-parent";
    let parent_tool_call_id = "r3-tc-orphan-remote-parent";
    let child_request_id = "child-orphan-remote-parent";

    upsert_target_behavior_with_cross_principal(
        db.node.as_ref(),
        &local_did,
        target_behavior_id,
        true,
    )
    .await;
    let parent_request_doc_id = create_remote_parent_request(
        db.node.as_ref(),
        remote_parent_did,
        parent_request_id,
        parent_session_id,
    )
    .await;
    create_orphan_cross_principal_tool_call(
        db.node.as_ref(),
        parent_request_id,
        &parent_request_doc_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
        // Reach parent validation after the recovery query's agent filter.
        &local_did,
        target_behavior_id,
        &local_did,
        target_behavior_id,
    )
    .await;

    let report = ToolCallLifecycle::recover_all(&db.node, &local_did)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 0);
    assert_no_child_request_for_tool(
        db.node.as_ref(),
        parent_tool_call_id,
        Duration::from_millis(0),
    )
    .await;
    // An absent remote parent is not a terminal parent. Keep the spawn recoverable.
    let tool = fetch_tool_call(db.node.as_ref(), parent_session_id, parent_tool_call_id).await;
    assert_eq!(
        tool.lifecycle_state.as_deref(),
        Some("running"),
        "recovery must leave a remote-parent orphan bridge running (parent absence is not a terminal)"
    );
}

async fn wait_for_subagent_source_subscription() {
    tokio::time::sleep(Duration::from_millis(250)).await;
}

async fn upsert_target_behavior_with_cross_principal(
    node: &EmbeddedNode,
    agent_did: &str,
    target_behavior_id: &str,
    allow_cross_principal: bool,
) {
    ensure_subagent_authorization(
        node,
        agent_did,
        target_behavior_id,
        &format!("{target_behavior_id}-xdep-tools"),
        vec![(
            target_behavior_id.to_string(),
            agent_did.to_string(),
            target_behavior_id.to_string(),
        )],
        true,
        true,
        Some(allow_cross_principal),
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn write_targeted_cross_principal_bridge(
    node: &EmbeddedNode,
    coordinator_did: &str,
    parent_request_id: &str,
    parent_tool_call_id: &str,
    child_request_id: &str,
    target_behavior_id: &str,
    target_agent_did: &str,
    parent_subagent_depth: u32,
) -> (String, String) {
    let parent_session_id = format!("session-{parent_tool_call_id}");
    let tool_call_key = format!("{parent_session_id}:{parent_tool_call_id}");
    let parent_request_doc_id = format!("remote-doc-{parent_request_id}");
    let args = serde_json::json!({
        "name": target_behavior_id,
        "agent_did": target_agent_did,
        "behavior_id": target_behavior_id,
        "prompt": "targeted cross-principal child prompt",
        "parent_subagent_depth": parent_subagent_depth,
    })
    .to_string();
    let delegated_input = gents_protocol::output::DelegatedToolInput {
        source: gents_protocol::output::PayloadRef {
            close_doc_id: format!("private-parent-close-{parent_tool_call_id}"),
            stream: 0,
        },
        arguments: args,
        parent_subagent_depth,
    };
    let delegated_input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(delegated_input).expect("serialize targeted delegated input"),
    )
    .expect("render targeted delegated input");
    let started_at = chrono::Utc::now().to_rfc3339();
    let deadline_at = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{parent_request_id}",
                request_doc_id: "{parent_request_doc_id}",
                session_id: "{parent_session_id}",
                agent_did: "{coordinator_did}",
                message_sequence: 1,
                tool_name: "spawn_subagent",
                tool_call_id: "{parent_tool_call_id}",
                delegated_input: {delegated_input},
                lifecycle_state: "running",
                started_at: "{started_at}",
                deadline_at: "{deadline_at}",
                child_request_id: "{child_request_id}",
                spawn_target_did: "{target_agent_did}",
                spawn_behavior_id: "{target_behavior_id}",
                await_mode: "background",
                cancel_policy: "cascade"
            }}) {{ _docID }}
        }}"#,
        tool_call_key = escape_graphql_string(&tool_call_key),
        parent_request_id = escape_graphql_string(parent_request_id),
        parent_request_doc_id = escape_graphql_string(&parent_request_doc_id),
        parent_session_id = escape_graphql_string(&parent_session_id),
        coordinator_did = escape_graphql_string(coordinator_did),
        parent_tool_call_id = escape_graphql_string(parent_tool_call_id),
        delegated_input = delegated_input,
        child_request_id = escape_graphql_string(child_request_id),
        target_agent_did = escape_graphql_string(target_agent_did),
        target_behavior_id = escape_graphql_string(target_behavior_id),
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create targeted AgentToolCall failed: {:?}",
        response.errors
    );
    let escaped_tool_call_key = escape_graphql_string(&tool_call_key);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ tool_call_key: {{ _eq: "{escaped_tool_call_key}" }} }},
                limit: 2
            ) {{ _docID }}
        }}"#
    );
    #[derive(Deserialize)]
    struct BridgeDocRow {
        #[serde(rename = "_docID")]
        doc_id: String,
    }
    let response = node.execute(&query).await;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| serde_json::from_value::<Vec<BridgeDocRow>>(value.clone()).ok())
        .unwrap_or_default();
    assert_eq!(rows.len(), 1, "targeted bridge must be exact");
    (parent_request_doc_id, rows[0].doc_id.clone())
}

async fn create_remote_parent_request(
    node: &EmbeddedNode,
    remote_agent_did: &str,
    parent_request_id: &str,
    parent_session_id: &str,
) -> String {
    let escaped_request_id = escape_graphql_string(parent_request_id);
    let escaped_agent_did = escape_graphql_string(remote_agent_did);
    let escaped_session_id = escape_graphql_string(parent_session_id);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                purpose: "normal",
                agent_did: "{escaped_agent_did}",
                behavior_id: "remote-parent-behavior",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "remote parent prompt",
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
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 2
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&query).await;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .expect("remote parent AgentRequest rows");
    assert_eq!(rows.len(), 1, "remote parent request must be exact");
    rows[0]
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .expect("remote parent AgentRequest _docID")
        .to_string()
}

#[allow(clippy::too_many_arguments)]
async fn write_cross_principal_bridge(
    node: &EmbeddedNode,
    bridge_author_did: &str,
    parent_request_id: &str,
    parent_request_doc_id: &str,
    parent_session_id: &str,
    parent_tool_call_id: &str,
    child_request_id: &str,
    target_behavior_id: &str,
    target_agent_did: &str,
) {
    write_cross_principal_bridge_with_spawn_target(
        node,
        bridge_author_did,
        parent_request_id,
        parent_request_doc_id,
        parent_session_id,
        parent_tool_call_id,
        child_request_id,
        target_behavior_id,
        target_agent_did,
        Some(target_agent_did),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn write_cross_principal_bridge_with_spawn_target(
    node: &EmbeddedNode,
    bridge_author_did: &str,
    parent_request_id: &str,
    parent_request_doc_id: &str,
    parent_session_id: &str,
    parent_tool_call_id: &str,
    child_request_id: &str,
    target_behavior_id: &str,
    target_agent_did: &str,
    spawn_target_did: Option<&str>,
) {
    let escaped_bridge_author_did = escape_graphql_string(bridge_author_did);
    let escaped_parent_request_id = escape_graphql_string(parent_request_id);
    let escaped_parent_request_doc_id = escape_graphql_string(parent_request_doc_id);
    let escaped_parent_session_id = escape_graphql_string(parent_session_id);
    let escaped_parent_tool_call_id = escape_graphql_string(parent_tool_call_id);
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let escaped_target_behavior_id = escape_graphql_string(target_behavior_id);
    let spawn_target_field = spawn_target_did
        .map(|did| format!(r#"spawn_target_did: "{}","#, escape_graphql_string(did)))
        .unwrap_or_else(|| "spawn_target_did: null,".to_string());
    let tool_call_key = format!("{escaped_parent_session_id}:{escaped_parent_tool_call_id}");
    let args = serde_json::json!({
        "name": target_behavior_id,
        "agent_did": target_agent_did,
        "behavior_id": target_behavior_id,
        "prompt": "cross-principal child prompt",
        "parent_subagent_depth": 0
    })
    .to_string();
    let delegated_input = gents_protocol::output::DelegatedToolInput {
        source: gents_protocol::output::PayloadRef {
            close_doc_id: format!("private-parent-close-{parent_tool_call_id}"),
            stream: 0,
        },
        arguments: args,
        parent_subagent_depth: 0,
    };
    let delegated_input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(delegated_input).expect("serialize delegated input"),
    )
    .expect("render delegated input");
    let started_at = chrono::Utc::now().to_rfc3339();
    let deadline_at = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{escaped_parent_request_id}",
                request_doc_id: "{escaped_parent_request_doc_id}",
                session_id: "{escaped_parent_session_id}",
                agent_did: "{escaped_bridge_author_did}",
                message_sequence: 1,
                tool_name: "spawn_subagent",
                tool_call_id: "{escaped_parent_tool_call_id}",
                delegated_input: {delegated_input},
                lifecycle_state: "running",
                started_at: "{started_at}",
                deadline_at: "{deadline_at}",
                child_request_id: "{escaped_child_request_id}",
                {spawn_target_field}
                spawn_behavior_id: "{escaped_target_behavior_id}",
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
        "create cross-principal AgentToolCall failed: {:?}",
        response.errors
    );
}

#[allow(clippy::too_many_arguments)]
async fn create_orphan_cross_principal_tool_call(
    node: &EmbeddedNode,
    parent_request_id: &str,
    parent_request_doc_id: &str,
    parent_session_id: &str,
    parent_tool_call_id: &str,
    child_request_id: &str,
    tool_call_agent_did: &str,
    target_name: &str,
    target_agent_did: &str,
    target_behavior_id: &str,
) {
    let escaped_parent_request_id = escape_graphql_string(parent_request_id);
    let escaped_parent_request_doc_id = escape_graphql_string(parent_request_doc_id);
    let escaped_parent_session_id = escape_graphql_string(parent_session_id);
    let escaped_parent_tool_call_id = escape_graphql_string(parent_tool_call_id);
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let escaped_target_agent_did = escape_graphql_string(target_agent_did);
    let escaped_target_behavior_id = escape_graphql_string(target_behavior_id);
    let tool_call_key = format!("{escaped_parent_session_id}:{escaped_parent_tool_call_id}");
    let args = serde_json::json!({
        "name": target_name,
        "agent_did": target_agent_did,
        "behavior_id": target_behavior_id,
        "prompt": "orphan cross-principal child prompt",
        "parent_subagent_depth": 0
    })
    .to_string();
    // A foreign-origin row misattributed to the local recovery principal is
    // adversarial imported input, not a locally accepted provider header.
    let delegated_input = gents_protocol::output::DelegatedToolInput {
        source: gents_protocol::output::PayloadRef {
            close_doc_id: format!("private-parent-close-{parent_tool_call_id}"),
            stream: 0,
        },
        arguments: args,
        parent_subagent_depth: 0,
    };
    let delegated_input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(delegated_input).expect("serialize imported delegated input"),
    )
    .expect("render imported delegated input");
    let started_at = chrono::Utc::now().to_rfc3339();
    let deadline_at = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{escaped_parent_request_id}",
                request_doc_id: "{escaped_parent_request_doc_id}",
                agent_did: "{agent_did}",
                session_id: "{escaped_parent_session_id}",
                message_sequence: 1,
                tool_name: "spawn_subagent",
                tool_call_id: "{escaped_parent_tool_call_id}",
                delegated_input: {delegated_input},
                lifecycle_state: "running",
                started_at: "{started_at}",
                deadline_at: "{deadline_at}",
                child_request_id: "{escaped_child_request_id}",
                spawn_target_did: "{escaped_target_agent_did}",
                spawn_behavior_id: "{escaped_target_behavior_id}",
                await_mode: "background",
                cancel_policy: "cascade",
                selected_service_id: null,
                selected_tool_name: null,
                tool_failure_class: null,
                latency_ms: null
            }}) {{ _docID }}
        }}"#,
        agent_did = escape_graphql_string(tool_call_agent_did),
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create orphan cross-principal AgentToolCall failed: {:?}",
        response.errors
    );
}
