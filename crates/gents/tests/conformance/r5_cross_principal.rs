//! R5 cross-principal subagent delegation conformance.
//!
//! Routing is keyed on agent DID identity (AgentPrincipal), not deployment
//! identity: the "cross" route spawns on a different principal's runtime and
//! the "same-principal" route falls back locally.

use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::lean_vocab_test::{lean_r5_cross_principal_cases, LeanR5CrossPrincipalCase};
use crate::support::first_optional_row;

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    request_doc_id: String,
    tool_name: String,
    tool_call_id: String,
    lifecycle_state: Option<String>,
    await_mode: Option<String>,
    cancel_policy: Option<String>,
    child_request_id: Option<String>,
    unclaimed_deadline_at: Option<String>,
}

pub(super) async fn generated_r5_cross_principal_cases_drive_production_dispatch() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "gents::trigger_engine::subagent_source=trace,gents::trigger_engine::production_materializer=debug,gents::agent::p2p_reconcile::enrollment_reconcile=debug",
        ))
        .try_init();
    let cases = lean_r5_cross_principal_cases();
    assert_eq!(
        cases.len(),
        2,
        "Lean should emit cross- and same-principal R5 rows"
    );

    for case in cases {
        assert_eq!(case.action.as_str(), "spawn_subagent", "{}", case.name);
        assert_eq!(case.await_mode.as_str(), "background", "{}", case.name);
        assert_eq!(case.cancel_policy.as_str(), "cascade", "{}", case.name);
        assert_eq!(
            case.child_request_id.as_str(),
            "runtime_generated",
            "{}",
            case.name
        );

        if case.cross_principal_routing_fired {
            drive_cross_principal_case(case).await;
        } else {
            drive_same_principal_case(case).await;
        }
    }
}

async fn drive_cross_principal_case(case: &LeanR5CrossPrincipalCase) {
    assert_eq!(case.route.as_str(), "cross_principal", "{}", case.name);
    assert_ne!(
        case.parent_principal, case.child_principal,
        "{} should cross principals",
        case.name
    );
    assert!(case.child_owned_by_target_principal, "{}", case.name);

    let parent_session_id = format!("{}-session", case.parent_request_id);
    let spawn_before = chrono::Utc::now();
    let runtime = crate::support::r5_cross_principal_runtime::boot_cross_principal_accepted_turn(
        crate::support::r5_cross_principal_runtime::R5AcceptedSpec {
            name: &case.name,
            parent_request_id: &case.parent_request_id,
            parent_session_id: &parent_session_id,
            parent_tool_call_id: &case.parent_tool_call_id,
            target_behavior_id: &case.target_behavior_id,
            prompt: "spawn the configured child",
            parent_subagent_depth: 0,
        },
    )
    .await;
    let bridge = wait_for_tool_call(
        runtime.parent_db.node.as_ref(),
        &parent_session_id,
        &case.parent_tool_call_id,
    )
    .await;
    let child_request_id = bridge
        .child_request_id
        .clone()
        .expect("accepted background bridge reserves a child request ID");
    let spawn_after = chrono::Utc::now();
    assert_bridge_matches_case(
        case,
        &bridge,
        &child_request_id,
        runtime.child_db.node.as_ref(),
    )
    .await;
    if let Some(value) = bridge.unclaimed_deadline_at.as_deref() {
        let deadline = chrono::DateTime::parse_from_rfc3339(value)
            .expect("persisted spawn deadline")
            .with_timezone(&chrono::Utc);
        // Bracket the owner call instead of assuming a maximum scheduler delay
        // between deadline computation and start_running's timestamp.
        let timeout = chrono::Duration::seconds(60); // Explicit fixture configuration below.
        assert!(
            (chrono::DateTime::from_timestamp(spawn_before.timestamp(), 0).unwrap() + timeout
                ..=spawn_after + timeout)
                .contains(&deadline),
            "{}: unclaimed deadline must use the configured spawn timeout",
            case.name,
        );
    }

    let replicated_bridge = wait_for_tool_call(
        runtime.child_db.node.as_ref(),
        &parent_session_id,
        &case.parent_tool_call_id,
    )
    .await;
    assert_bridge_matches_case(
        case,
        &replicated_bridge,
        &child_request_id,
        runtime.child_db.node.as_ref(),
    )
    .await;
    assert!(
        fetch_child_request_optional(runtime.child_db.node.as_ref(), &case.parent_request_id)
            .await
            .is_none(),
        "{}: the targeted bridge must not drag the coordinator parent request to B",
        case.name
    );

    let child = wait_for_child_request(runtime.child_db.node.as_ref(), &child_request_id).await;
    assert_child_matches_case(case, &child, &child_request_id);
    let child_agent_did = runtime.child_agent_did.clone();
    let parent_agent_did = runtime.parent_db.node_identity.did().to_owned();
    assert_ne!(
        parent_agent_did, child_agent_did,
        "{}: cross-principal route must use distinct runtime DIDs",
        case.name
    );
    assert_eq!(
        child.agent_did.as_deref(),
        Some(child_agent_did.as_str()),
        "{}: cross-principal child must be locally owned by B",
        case.name
    );
    assert_eq!(
        child.requester_did.as_deref(),
        Some(parent_agent_did.as_str()),
        "{}: the child return route must name the coordinator",
        case.name
    );
    assert_eq!(
        child.runtime_issuer_did.as_deref(),
        Some(child_agent_did.as_str())
    );
    assert_eq!(
        child.runtime_bridge_author_did.as_deref(),
        Some(parent_agent_did.as_str())
    );
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(case.parent_request_id.as_str())
    );
    assert_eq!(
        child.caused_by_parent_request_doc_id.as_deref(),
        Some(bridge.request_doc_id.as_str())
    );
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        Some(case.parent_tool_call_id.as_str())
    );
    assert_eq!(
        child.caused_by_parent_tool_call_doc_id.as_deref(),
        Some(bridge.doc_id.as_str())
    );
    if let Some(replica) =
        fetch_child_request_optional(runtime.parent_db.node.as_ref(), &child_request_id).await
    {
        assert_eq!(
            replica.doc_id, child.doc_id,
            "{}: A has a different physical child",
            case.name
        );
        assert_eq!(
            replica.runtime_issuer_did, child.runtime_issuer_did,
            "{}: A must not author B's child",
            case.name
        );
        assert_eq!(
            replica.caused_by_parent_tool_call_doc_id,
            child.caused_by_parent_tool_call_doc_id
        );
    }

    runtime.shutdown().await;
}

async fn drive_same_principal_case(case: &LeanR5CrossPrincipalCase) {
    assert_eq!(case.route.as_str(), "same_principal", "{}", case.name);
    assert_eq!(
        case.parent_principal, case.child_principal,
        "{} should stay within one principal",
        case.name
    );
    assert!(case.same_principal_fallback, "{}", case.name);

    let parent_session_id = format!("{}-session", case.parent_request_id);
    let runtime = crate::support::r5_cross_principal_runtime::boot_same_principal_accepted_turn(
        crate::support::r5_cross_principal_runtime::R5AcceptedSpec {
            name: &case.name,
            parent_request_id: &case.parent_request_id,
            parent_session_id: &parent_session_id,
            parent_tool_call_id: &case.parent_tool_call_id,
            target_behavior_id: &case.target_behavior_id,
            prompt: "spawn the local configured child",
            parent_subagent_depth: 0,
        },
    )
    .await;
    let bridge = wait_for_tool_call(
        runtime.db.node.as_ref(),
        &parent_session_id,
        &case.parent_tool_call_id,
    )
    .await;
    let child_request_id = bridge
        .child_request_id
        .clone()
        .expect("accepted local bridge reserves a child request ID");
    assert_bridge_matches_case(case, &bridge, &child_request_id, runtime.db.node.as_ref()).await;

    let child = wait_for_child_request(runtime.db.node.as_ref(), &child_request_id).await;
    assert_child_matches_case(case, &child, &child_request_id);
    assert_eq!(
        child.agent_did.as_deref(),
        Some(runtime.db.node_identity.did()),
        "{}: same-principal fallback should keep child ownership local",
        case.name
    );
    runtime.shutdown().await;
}

async fn fetch_tool_call_optional(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Option<ToolCallRow> {
    let session_id = escape_graphql_string(session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_call_id: {{ _eq: "{tool_call_id}" }}
                }},
                limit: 1
            ) {{
                _docID
                request_id
                request_doc_id
                tool_name
                tool_call_id
                lifecycle_state
                await_mode
                cancel_policy
                child_request_id
                tool_failure_class
                unclaimed_deadline_at
            }}
        }}"#
    );
    first_optional_row(&node.execute(&query).await, "AgentToolCall")
}

async fn fetch_child_request_optional(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Option<AgentRequestRow> {
    let child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{child_request_id}" }} }}, limit: 1) {{
                _docID
                request_id
                agent_did
                requester_did
                runtime_issuer_did
                runtime_bridge_author_did
                lifecycle_state
                behavior_id
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    first_optional_row(&node.execute(&query).await, "AgentRequest")
}

async fn wait_for_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(tool_call) = fetch_tool_call_optional(node, session_id, tool_call_id).await {
            // Provider publication first creates Pending. The conformance
            // assertion observes dispatch, so wait for the owner transition
            // instead of racing it and treating Pending as a route failure.
            if tool_call.lifecycle_state.as_deref() != Some("pending") {
                return tool_call;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let diagnostic = agent_tool_call_diagnostic(node).await;
            let p2p = p2p_diagnostic(node).await;
            panic!("tool call {tool_call_id} was not replicated; {diagnostic}; {p2p}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn p2p_diagnostic(node: &EmbeddedNode) -> String {
    let Some(p2p) = node.p2p() else {
        return "p2p=disabled".to_owned();
    };
    let peers = tokio::time::timeout(Duration::from_secs(2), p2p.connected_peers()).await;
    let replicators = tokio::time::timeout(Duration::from_secs(2), p2p.get_replicators()).await;
    format!("connected_peers={peers:?}, replicators={replicators:?}")
}

async fn wait_for_child_request(node: &EmbeddedNode, child_request_id: &str) -> AgentRequestRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(child) = fetch_child_request_optional(node, child_request_id).await {
            return child;
        }
        if tokio::time::Instant::now() >= deadline {
            let diagnostic = agent_request_diagnostic(node).await;
            let admission = subagent_admission_diagnostic(node).await;
            panic!(
                "child request {child_request_id} was not materialized; {diagnostic}; {admission}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn subagent_admission_diagnostic(node: &EmbeddedNode) -> String {
    let response = node
        .execute(
            r#"{
                AgentToolCall {
                    _docID request_id request_doc_id agent_did requester_did
                    session_id tool_call_id tool_name lifecycle_state
                    child_request_id spawn_target_did spawn_behavior_id
                    delegated_input
                }
                runningBridges: AgentToolCall(filter: {
                    lifecycle_state: { _eq: "running" },
                    child_request_id: { _ne: "" }
                }) { _docID child_request_id }
                AgentBehavior { behavior_id agent_did enabled }
                AgentBehaviorReadiness { agent_did snapshot_json updated_at }
                AgentContext { context_id agent_did tools_id }
                Tools { tools_id agent_did subagents }
                PeerPairingDesired { peer_id agent_did collections }
                NetworkEnrollmentRequest {
                    request_id admin_did candidate_did owner_agent expires_at
                }
                NetworkAuthorizationRevision {
                    request_id member_did member_peer owner_agent sequence
                    authorization_expires_at kind
                }
                AgentMessage { _docID request_doc_id agent_did requester_did session_id publication blocks }
            }"#,
        )
        .await;
    format!(
        "subagent admission errors={:?} data={:?}",
        response.errors, response.data
    )
}

async fn agent_request_diagnostic(node: &EmbeddedNode) -> String {
    let response = node
        .execute(
            r#"{
                AgentRequest {
                    request_id
                    agent_did
                    requester_did
                    behavior_id
                    caused_by_parent_request_id
                    caused_by_parent_tool_call_id
                    caused_by_trigger_id
                    caused_by_trigger_kind
                }
            }"#,
        )
        .await;
    format!(
        "AgentRequest errors={:?} data={:?}",
        response.errors, response.data
    )
}

async fn agent_tool_call_diagnostic(node: &EmbeddedNode) -> String {
    let response = node
        .execute(
            r#"{
                AgentToolCall {
                    request_id
                    session_id
                    tool_name
                    tool_call_id
                    lifecycle_state
                    child_request_id
                }
            }"#,
        )
        .await;
    format!(
        "AgentToolCall errors={:?} data={:?}",
        response.errors, response.data
    )
}

async fn assert_bridge_matches_case(
    case: &LeanR5CrossPrincipalCase,
    bridge: &ToolCallRow,
    child_request_id: &str,
    child_node: &EmbeddedNode,
) {
    assert!(case.parent_trigger_persisted, "{}", case.name);
    assert_eq!(
        bridge.request_id, case.parent_request_id,
        "{}: bridge parent request",
        case.name
    );
    assert_eq!(
        bridge.tool_call_id, case.parent_tool_call_id,
        "{}: bridge tool id",
        case.name
    );
    assert_eq!(bridge.tool_name, "spawn_subagent", "{}", case.name);
    match bridge.lifecycle_state.as_deref() {
        Some("running") => {}
        Some("completed") => {
            let child = wait_for_child_request(child_node, child_request_id).await;
            assert_eq!(
                child.lifecycle_state,
                Some(gents_protocol::request_lifecycle::RequestLifecycleState::Completed),
                "{}: bridge completed before its exact child completed",
                case.name
            );
        }
        other => panic!("{}: bridge has unexpected lifecycle {other:?}", case.name),
    }
    assert_eq!(
        bridge.await_mode.as_deref(),
        Some(case.await_mode.as_str()),
        "{}",
        case.name
    );
    assert_eq!(
        bridge.cancel_policy.as_deref(),
        Some(case.cancel_policy.as_str()),
        "{}",
        case.name
    );
    assert_eq!(
        bridge.child_request_id.as_deref(),
        Some(child_request_id),
        "{}: bridge child_request_id",
        case.name
    );
    assert_eq!(
        bridge.unclaimed_deadline_at.is_some(),
        case.unclaimed_deadline_set,
        "{}: unclaimed deadline",
        case.name
    );
}

fn assert_child_matches_case(
    case: &LeanR5CrossPrincipalCase,
    child: &AgentRequestRow,
    child_request_id: &str,
) {
    assert!(case.child_materialized, "{}", case.name);
    assert_eq!(child.request_id, child_request_id, "{}", case.name);
    assert_eq!(
        child.behavior_id.as_deref(),
        Some(case.target_behavior_id.as_str()),
        "{}: child target behavior",
        case.name
    );
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        case.caused_by_parent_request_id_matches
            .then_some(case.parent_request_id.as_str()),
        "{}: parent request linkage",
        case.name
    );
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        case.caused_by_parent_tool_call_id_matches
            .then_some(case.parent_tool_call_id.as_str()),
        "{}: parent tool linkage",
        case.name
    );
    assert_eq!(
        child.caused_by_trigger_id.as_deref(),
        Some(case.parent_tool_call_id.as_str()),
        "{}: trigger id linkage",
        case.name
    );
    assert_eq!(
        child.caused_by_trigger_kind.as_deref(),
        Some(case.caused_by_trigger_kind.as_str()),
        "{}: trigger kind linkage",
        case.name
    );
}
