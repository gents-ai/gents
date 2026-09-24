use std::time::{Duration, Instant};

use gents::background_completion::{observe_cancel_cascade_ack, CancelAckOutcome};
use gents::default_behavior_id_for_agent;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::{
    AwaitMode, CancelCause, CancelPolicy, CascadeDispatch, ToolCallLifecycle,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use serde_json::json;

use crate::lean_vocab_test::lean_cancel_propagation_cases;
use crate::support::r5_cross_principal_runtime::{
    boot_cross_principal_accepted_turn, R5AcceptedSpec,
};
use crate::support::{first_optional_row, set_request_lifecycle_state, test_p2p_db};

#[derive(Debug, Deserialize)]
struct BridgeRow {
    lifecycle_state: Option<String>,
    child_request_id: Option<String>,
    spawn_target_did: Option<String>,
    cancel_cascade_intent_at: Option<String>,
    cancel_pending_remote_ack: Option<bool>,
}

pub(super) async fn cancel_propagation_cases_drive_production_interrupt() {
    let cases = lean_cancel_propagation_cases();
    assert_eq!(
        cases.len(),
        1,
        "Lean should emit one cancel propagation row"
    );

    let case = &cases[0];
    assert_eq!(
        case.name,
        "cancel_propagates_across_declarative_subagent_legs"
    );
    assert_eq!(case.route, "declarative_subagent_pairing");
    assert_eq!(case.action, "cancel_bridge");
    assert_eq!(case.parent_principal, "coordinator");
    assert_eq!(case.child_principal, "worker");
    assert_eq!(case.bridge_collection, "AgentToolCall");
    assert_eq!(case.child_request_collection, "AgentRequest");
    assert!(case.cancel_intent_written_on_bridge);
    assert!(case.bridge_cancel_replicates_to_host);
    assert!(case.host_interrupts_child);
    assert!(case.child_interrupt_intent_replicates_to_coordinator);
    assert!(case.cancel_ack_returns_to_coordinator);
    assert!(case.no_third_party_rows);

    drive_declarative_cancel_propagation().await;
}

async fn drive_declarative_cancel_propagation() {
    let parent_request_id = "cancel-propagation-parent";
    let parent_session_id = "cancel-propagation-parent-session";
    let parent_tool_call_id = "cancel-propagation-bridge";
    let runtime = boot_cross_principal_accepted_turn(R5AcceptedSpec {
        name: "cancel-propagation",
        parent_request_id,
        parent_session_id,
        parent_tool_call_id,
        target_behavior_id: "cancel-propagation-worker",
        prompt: "parent work",
        parent_subagent_depth: 0,
        hold_child_provider: true,
    })
    .await;
    let coord_node = runtime.parent_db.node.clone();
    let host_node = runtime.child_db.node.clone();
    let coord_did = runtime.parent_db.node_identity.did().to_owned();
    let host_did = runtime.child_agent_did.clone();
    let replicated_bridge = wait_for_bridge(
        host_node.as_ref(),
        parent_session_id,
        parent_tool_call_id,
        Duration::from_secs(120),
    )
    .await;
    let child_request_id = replicated_bridge
        .child_request_id
        .clone()
        .expect("accepted bridge must bind its physical child request");
    assert_eq!(
        replicated_bridge.spawn_target_did.as_deref(),
        Some(host_did.as_str())
    );
    assert_eq!(
        replicated_bridge.child_request_id.as_deref(),
        Some(child_request_id.as_str())
    );
    assert!(
        fetch_request(host_node.as_ref(), parent_request_id)
            .await
            .is_none(),
        "coordinator parent request must not replicate to the host"
    );
    let child_prompt = "child prompt for cancel-propagation";
    tokio::time::timeout(Duration::from_secs(30), async {
        while runtime.child_provider_observed_requests(child_prompt) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("worker did not reach its paused provider response before cancellation");
    let owned_child = fetch_request(host_node.as_ref(), &child_request_id)
        .await
        .expect("worker child request exists before cancellation");
    assert!(
        matches!(
            owned_child.lifecycle_state,
            Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
        ),
        "worker must own the child request while its provider is paused"
    );
    let coord_bridge_before_cancel = wait_for_bridge(
        coord_node.as_ref(),
        parent_session_id,
        parent_tool_call_id,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        coord_bridge_before_cancel.lifecycle_state.as_deref(),
        Some("running")
    );

    let mut bridge =
        ToolCallLifecycle::load(coord_node.clone(), parent_session_id, parent_tool_call_id)
            .await
            .expect("load accepted bridge")
            .expect("accepted bridge exists");
    let dispatch = bridge
        .cancel_during_run_with_cascade_dispatch(CancelCause::UserCancelled, &coord_did)
        .await
        .expect("cancel bridge with remote cascade dispatch");
    let coord_bridge_after_cancel =
        fetch_bridge(coord_node.as_ref(), parent_session_id, parent_tool_call_id).await;
    assert!(
        matches!(dispatch, Some(CascadeDispatch::RemoteIntentWritten)),
        "expected remote cascade dispatch, got {dispatch:?}; before={coord_bridge_before_cancel:?}; after={coord_bridge_after_cancel:?}"
    );

    let host_bridge = wait_for_bridge_cancel_intent(
        host_node.as_ref(),
        parent_session_id,
        parent_tool_call_id,
        Duration::from_secs(120),
    )
    .await;
    assert_eq!(host_bridge.lifecycle_state.as_deref(), Some("cancelled"));
    assert!(host_bridge.cancel_cascade_intent_at.is_some());
    assert_eq!(host_bridge.cancel_pending_remote_ack, Some(true));

    let interrupted_on_host = wait_for_interrupt_requested_at(
        host_node.as_ref(),
        &child_request_id,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(
        interrupted_on_host.agent_did.as_deref(),
        Some(host_did.as_str())
    );
    assert!(
        matches!(
            interrupted_on_host.lifecycle_state,
            Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
        ),
        "host must latch interruption while the accepted child remains owned and nonterminal; observed {:?}",
        interrupted_on_host.lifecycle_state
    );

    let interrupted_on_coord = wait_for_interrupt_requested_at(
        coord_node.as_ref(),
        &child_request_id,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(
        interrupted_on_coord.agent_did.as_deref(),
        Some(host_did.as_str())
    );

    let ack_outcomes = observe_cancel_cascade_ack(coord_node.clone(), &coord_did)
        .await
        .expect("observe cancel ack on coordinator");
    assert!(
        ack_outcomes.iter().any(|outcome| matches!(
            outcome,
            CancelAckOutcome::Acked {
                parent_tool_call_id: acked_tool_call_id
            } if acked_tool_call_id == parent_tool_call_id
        )),
        "expected cancel ack for {parent_tool_call_id}, got {ack_outcomes:?}"
    );
    let acked_bridge = wait_for_bridge_ack_cleared(
        coord_node.as_ref(),
        parent_session_id,
        parent_tool_call_id,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(acked_bridge.lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(acked_bridge.cancel_pending_remote_ack, Some(false));

    assert_no_third_party_rows(coord_node.as_ref(), &coord_did, &host_did).await;
    assert_no_third_party_rows(host_node.as_ref(), &coord_did, &host_did).await;

    runtime.shutdown().await;
}

/// The end-to-end propagation above only ever observes the terminal `Acked`
/// outcome of `observe_cancel_cascade_ack`. The same owner also classifies
/// bridges whose child has not converged: `Pending` while the remote intent
/// is young, and `Stuck` — with a durable `stuck_since` stamp — once the
/// intent exceeds the threshold. A regression that reported `Stuck` without
/// persisting `stuck_since`, or cleared the ack flag for a not-yet-terminal
/// child, would pass the end-to-end fixture. Drives the real observer over a
/// locally-owned parent per outcome; no child row means "not done", exactly
/// the pre-replication state the observer must tolerate.
#[tokio::test]
async fn cancel_ack_observer_reports_pending_stuck_and_acked_outcomes() {
    let db = test_p2p_db("cancel-ack-outcomes").await;
    let local_did = db.node_identity.did().to_string();
    let behavior_id = default_behavior_id_for_agent(&local_did);

    // Sub-case A: young intent, child absent -> Pending, ack flag untouched.
    create_processing_request(
        db.node.as_ref(),
        "ack-pending-parent",
        "ack-pending-session",
        &local_did,
        &behavior_id,
        "pending ack work",
        0,
        None,
        None,
        None,
    )
    .await;
    let pending_parent_doc_id = fetch_request(db.node.as_ref(), "ack-pending-parent")
        .await
        .expect("pending-ack parent request")
        .doc_id
        .expect("pending-ack parent _docID");
    write_cancel_pending_bridge(
        db.node.as_ref(),
        "ack-pending-parent",
        &pending_parent_doc_id,
        "ack-pending-session",
        "ack-pending-bridge",
        &local_did,
        "ack-pending-child",
        &chrono::Utc::now().to_rfc3339(),
    )
    .await;

    // Sub-case B: ancient intent, child absent -> Stuck with durable
    // stuck_since, ack flag still pending.
    create_processing_request(
        db.node.as_ref(),
        "ack-stuck-parent",
        "ack-stuck-session",
        &local_did,
        &behavior_id,
        "stuck ack work",
        0,
        None,
        None,
        None,
    )
    .await;
    let stuck_parent_doc_id = fetch_request(db.node.as_ref(), "ack-stuck-parent")
        .await
        .expect("stuck-ack parent request")
        .doc_id
        .expect("stuck-ack parent _docID");
    write_cancel_pending_bridge(
        db.node.as_ref(),
        "ack-stuck-parent",
        &stuck_parent_doc_id,
        "ack-stuck-session",
        "ack-stuck-bridge",
        &local_did,
        "ack-stuck-child",
        "2020-01-01T00:00:00Z",
    )
    .await;

    // Sub-case C: child row exists locally and is terminal -> Acked clears
    // the pending flag and any stuck stamp.
    create_processing_request(
        db.node.as_ref(),
        "ack-acked-parent",
        "ack-acked-session",
        &local_did,
        &behavior_id,
        "acked work",
        0,
        None,
        None,
        None,
    )
    .await;
    let acked_parent_doc_id = fetch_request(db.node.as_ref(), "ack-acked-parent")
        .await
        .expect("acked parent request")
        .doc_id
        .expect("acked parent _docID");
    write_cancel_pending_bridge(
        db.node.as_ref(),
        "ack-acked-parent",
        &acked_parent_doc_id,
        "ack-acked-session",
        "ack-acked-bridge",
        &local_did,
        "ack-acked-child",
        "2020-01-01T00:00:00Z",
    )
    .await;
    exec(
        db.node.as_ref(),
        r#"mutation { update_AgentToolCall(
            filter: { tool_call_id: { _eq: "ack-acked-bridge" } },
            input: {
                stuck_since: "2020-01-01T00:01:00Z",
                started_at: "2026-05-15T00:00:00Z",
                deadline_at: "2026-05-15T00:05:00Z",
                completed_at: "2026-05-15T00:01:00Z",
                cancel_cascade_intent_at: "2020-01-01T00:00:00Z"
            }
        ) { _docID } }"#,
        "seed previously stuck ack bridge",
    )
    .await;
    assert!(fetch_ack_tool_row(db.node.as_ref(), "ack-acked-bridge")
        .await
        .stuck_since
        .is_some());
    // Only a child carrying the bridge's physical lineage acknowledges it.
    let acked_bridge_doc_id = db
        .node
        .execute(r#"{ AgentToolCall(filter: { tool_call_id: { _eq: "ack-acked-bridge" } }) { _docID } }"#)
        .await
        .data
        .expect("acked bridge row")["AgentToolCall"][0]["_docID"]
        .as_str()
        .expect("acked bridge _docID")
        .to_owned();
    let now = chrono::Utc::now().to_rfc3339();
    exec(
        db.node.as_ref(),
        &format!(
            r#"mutation {{ create_AgentRequest(input: {{
                request_id: "ack-acked-child", agent_did: "{local_did}",
                behavior_id: "{behavior_id}", session_id: "ack-acked-child-session",
                retry_parent_request: "", retry_root_request: "ack-acked-child",
                superseded_by_request: "", content: "child work",
                lifecycle_state: "processing", backend_id: "",
                execution_origin: "interactive", failure_reason: "",
                created_at: "{now}", retry_count: 0, max_retries: 3, subagent_depth: 1,
                caused_by_parent_request_id: "ack-acked-parent",
                caused_by_parent_request_doc_id: "{acked_parent_doc_id}",
                caused_by_parent_tool_call_id: "ack-acked-bridge",
                caused_by_parent_tool_call_doc_id: "{acked_bridge_doc_id}"
            }}) {{ _docID }} }}"#,
            local_did = escape_graphql_string(&local_did),
            behavior_id = escape_graphql_string(&behavior_id),
        ),
        "create acked child with bridge lineage",
    )
    .await;
    let acked_child_doc_id = fetch_request(db.node.as_ref(), "ack-acked-child")
        .await
        .expect("acked child request")
        .doc_id
        .expect("acked child _docID");
    set_request_lifecycle_state(db.node.as_ref(), &acked_child_doc_id, "failed").await;

    let outcomes = observe_cancel_cascade_ack(db.node.clone(), &local_did)
        .await
        .expect("observe cancel ack outcomes");
    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(
                outcome,
                CancelAckOutcome::Pending { parent_tool_call_id } if parent_tool_call_id == "ack-pending-bridge"
            )),
        "a young remote intent with no converged child must classify as Pending: {outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(
                outcome,
                CancelAckOutcome::Stuck { parent_tool_call_id, .. } if parent_tool_call_id == "ack-stuck-bridge"
            )),
        "an intent past the stuck threshold must classify as Stuck: {outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(
                outcome,
                CancelAckOutcome::Acked { parent_tool_call_id } if parent_tool_call_id == "ack-acked-bridge"
            )),
        "a terminal local child must classify as Acked: {outcomes:?}"
    );

    let pending_tool = fetch_ack_tool_row(db.node.as_ref(), "ack-pending-bridge").await;
    assert_eq!(pending_tool.cancel_pending_remote_ack, Some(true));
    assert!(
        pending_tool.stuck_since.is_none(),
        "the Pending outcome must not stamp stuck_since"
    );

    let stuck_tool = fetch_ack_tool_row(db.node.as_ref(), "ack-stuck-bridge").await;
    assert_eq!(
        stuck_tool.cancel_pending_remote_ack,
        Some(true),
        "the Stuck outcome must keep the ack flag pending"
    );
    assert!(
        stuck_tool.stuck_since.is_some(),
        "the Stuck outcome must durably stamp stuck_since for stuck-bridge observability"
    );

    let acked_tool = fetch_ack_tool_row(db.node.as_ref(), "ack-acked-bridge").await;
    assert_eq!(
        acked_tool.cancel_pending_remote_ack,
        Some(false),
        "the Acked outcome must clear the pending ack flag"
    );
    assert!(
        acked_tool.stuck_since.is_none(),
        "the Acked outcome must clear any stuck stamp"
    );
}

async fn write_cancel_pending_bridge(
    node: &EmbeddedNode,
    request_id: &str,
    request_doc_id: &str,
    session_id: &str,
    tool_call_id: &str,
    agent_did: &str,
    child_request_id: &str,
    intent_at: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let request_doc_id = escape_graphql_string(request_doc_id);
    let session_id = escape_graphql_string(session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let agent_did = escape_graphql_string(agent_did);
    let child_request_id = escape_graphql_string(child_request_id);
    let intent_at = escape_graphql_string(intent_at);
    let started_at = escape_graphql_string("2026-05-15T00:00:00Z");
    let deadline_at = escape_graphql_string("2026-05-15T00:05:00Z");
    let completed_at = escape_graphql_string("2026-05-15T00:01:00Z");
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{request_id}:{tool_call_id}",
                request_id: "{request_id}",
                request_doc_id: "{request_doc_id}",
                session_id: "{session_id}",
                agent_did: "{agent_did}",
                message_sequence: 1,
                tool_name: "spawn_subagent",
                tool_call_id: "{tool_call_id}",
                args: "{{}}",
                status: "completed",
                lifecycle_state: "cancelled",
                cancel_cause: "interrupted",
                started_at: "{started_at}",
                deadline_at: "{deadline_at}",
                completed_at: "{completed_at}",
                await_mode: "background",
                cancel_policy: "cascade",
                child_request_id: "{child_request_id}",
                cancel_cascade_intent_at: "{intent_at}",
                cancel_pending_remote_ack: true
            }}) {{ _docID }}
        }}"#
    );
    exec(node, &mutation, "write cancel-pending bridge fixture").await;
}

#[derive(Debug, Deserialize)]
struct AckToolRow {
    cancel_pending_remote_ack: Option<bool>,
    stuck_since: Option<String>,
}

async fn fetch_ack_tool_row(node: &EmbeddedNode, tool_call_id: &str) -> AckToolRow {
    let tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{tool_call_id}" }} }}, limit: 1) {{
                cancel_pending_remote_ack
                stuck_since
            }}
        }}"#
    );
    first_optional_row(&node.execute(&query).await, "AgentToolCall")
        .expect("cancel-pending bridge fixture row")
}

#[allow(clippy::too_many_arguments)]
async fn create_processing_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    behavior_id: &str,
    content: &str,
    subagent_depth: u32,
    parent_request_id: Option<&str>,
    parent_tool_call_id: Option<&str>,
    requester_did: Option<&str>,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let agent_did = escape_graphql_string(agent_did);
    let behavior_id = escape_graphql_string(behavior_id);
    let content = escape_graphql_string(content);
    let created_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let deadline =
        escape_graphql_string(&(chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339());
    let parent_request = graphql_nullable_string(parent_request_id);
    let parent_tool = graphql_nullable_string(parent_tool_call_id);
    let trigger_id = graphql_nullable_string(parent_tool_call_id);
    let requester_did = graphql_nullable_string(requester_did);
    let trigger_kind = if parent_tool_call_id.is_some() {
        "\"subagent\"".to_string()
    } else {
        "null".to_string()
    };
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                purpose: "normal",
                agent_did: "{agent_did}",
                requester_did: {requester_did},
                behavior_id: "{behavior_id}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "{content}",
                lifecycle_state: "processing",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{created_at}",
                deadline: "{deadline}",
                retry_count: 0,
                max_retries: 3,
                subagent_depth: {subagent_depth},
                caused_by_parent_request_id: {parent_request},
                caused_by_parent_tool_call_id: {parent_tool},
                caused_by_trigger_id: {trigger_id},
                caused_by_trigger_kind: {trigger_kind}
            }}) {{ _docID }}
        }}"#
    );
    exec(node, &mutation, "create processing AgentRequest").await;
}

fn graphql_nullable_string(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("\"{}\"", escape_graphql_string(value)),
        None => "null".to_string(),
    }
}

async fn wait_for_interrupt_requested_at(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> AgentRequestRow {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    loop {
        if let Some(row) = fetch_request(node, request_id).await {
            if row
                .interrupt_requested_at
                .as_deref()
                .is_some_and(|s| !s.is_empty())
            {
                return row;
            }
            last = Some(row);
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for AgentRequest({request_id}) interrupt; last={last:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn fetch_request(node: &EmbeddedNode, request_id: &str) -> Option<AgentRequestRow> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                _docID
                request_id
                agent_did
                lifecycle_state
                interrupt_requested_at
            }}
        }}"#
    );
    first_optional_row(&node.execute(&query).await, "AgentRequest")
}

async fn wait_for_bridge(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
    timeout: Duration,
) -> BridgeRow {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(row) = fetch_bridge(node, session_id, tool_call_id).await {
            return row;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for AgentToolCall({session_id}/{tool_call_id})");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_bridge_cancel_intent(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
    timeout: Duration,
) -> BridgeRow {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    loop {
        if let Some(row) = fetch_bridge(node, session_id, tool_call_id).await {
            if row
                .cancel_cascade_intent_at
                .as_deref()
                .is_some_and(|s| !s.is_empty())
            {
                return row;
            }
            last = Some(row);
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for AgentToolCall({session_id}/{tool_call_id}) cancel intent; last={last:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_bridge_ack_cleared(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
    timeout: Duration,
) -> BridgeRow {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    loop {
        if let Some(row) = fetch_bridge(node, session_id, tool_call_id).await {
            if row.cancel_pending_remote_ack == Some(false) {
                return row;
            }
            last = Some(row);
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for AgentToolCall({session_id}/{tool_call_id}) cancel ack; last={last:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn fetch_bridge(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Option<BridgeRow> {
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
                lifecycle_state
                child_request_id
                spawn_target_did
                cancel_cascade_intent_at
                cancel_pending_remote_ack
            }}
        }}"#
    );
    first_optional_row(&node.execute(&query).await, "AgentToolCall")
}

async fn assert_no_third_party_rows(node: &EmbeddedNode, coord_did: &str, host_did: &str) {
    let query = r#"{
        AgentRequest { agent_did }
        AgentToolCall { agent_did spawn_target_did }
    }"#;
    let response = node.execute(query).await;
    assert!(
        !response.has_errors(),
        "third-party row query failed: {:?}",
        response.errors
    );
    let data = response.data.expect("third-party query data");
    for row in data
        .get("AgentRequest")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(agent_did) = row.get("agent_did").and_then(serde_json::Value::as_str) else {
            continue;
        };
        assert!(
            agent_did == coord_did || agent_did == host_did,
            "unexpected AgentRequest agent_did {agent_did}"
        );
    }
    for row in data
        .get("AgentToolCall")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(agent_did) = row.get("agent_did").and_then(serde_json::Value::as_str) {
            assert!(
                agent_did == coord_did || agent_did == host_did,
                "unexpected AgentToolCall agent_did {agent_did}"
            );
        }
        if let Some(target_did) = row
            .get("spawn_target_did")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
        {
            assert!(
                target_did == coord_did || target_did == host_did,
                "unexpected AgentToolCall spawn_target_did {target_did}"
            );
        }
    }
}

async fn exec(node: &EmbeddedNode, statement: &str, context: &str) {
    let response = node.execute(statement).await;
    assert!(
        !response.has_errors(),
        "{context} failed: {:?}\n{statement}",
        response.errors
    );
}
