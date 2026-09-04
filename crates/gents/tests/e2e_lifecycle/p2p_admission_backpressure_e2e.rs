//! Multi-node e2e for #630 hub admission under a constrained push-worker bound.
//!
//! The TLA+ `P2PBackpressure` model makes `HealthyPeersDeliver` load-bearing on
//! `PushWorkers` and timeout-slot release. This test exercises the shipping
//! binding: both nodes start with `max_concurrent_push_tasks = 1` (the model's
//! one-worker hub shape) and a real AgentRequest still converges owner → peer
//! over PushLog. That is the operational acceptance check for the operator
//! knobs: tight fan-out does not permanently stall healthy delivery when the
//! peer is responsive.
//!
//! Out of scope here (needs defradb SyncDiagnostics export or fault injection):
//! nonresponsive peer holding the only permit, Bitswap-stalled pending roots,
//! gossip send-loop death.

use std::time::{Duration, Instant};

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::support::p2p_waits::{wait_for_connected_peer, wait_for_listen_addr};
use crate::support::{test_p2p_db_with_admission, TestP2pAdmission};

const OWNER_DID: &str = "did:test:admission-p2p-owner";
const BEHAVIOR_ID: &str = "admission-p2p-behavior";

async fn install_one_way_replicator(
    sender: &EmbeddedNode,
    receiver: &EmbeddedNode,
    collections: &[&str],
) {
    let sender_addr = wait_for_listen_addr(sender).await;
    let receiver_addr = wait_for_listen_addr(receiver).await;
    let sender_p2p = sender.p2p().expect("sender p2p");
    let receiver_p2p = receiver.p2p().expect("receiver p2p");

    sender_p2p
        .connect_peer(&receiver_addr)
        .await
        .expect("connect sender to receiver");
    wait_for_connected_peer(sender).await;
    wait_for_connected_peer(receiver).await;

    let collection_names = collections
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    sender_p2p
        .add_collections(collection_names.clone())
        .await
        .expect("add sender p2p collections");
    receiver_p2p
        .add_collections(collection_names.clone())
        .await
        .expect("add receiver p2p collections");
    receiver_p2p
        .add_replicator(
            collection_names.clone(),
            Some(&sender_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("authorize sender as receiver-side replicator");
    sender_p2p
        .add_replicator(
            collection_names,
            Some(&receiver_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("install sender to receiver replicator");
}

async fn create_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    lifecycle_state: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let agent_did = escape_graphql_string(agent_did);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{BEHAVIOR_ID}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "admission backpressure e2e",
                lifecycle_state: "{lifecycle_state}",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "2026-07-09T00:00:00Z",
                retry_count: 0,
                max_retries: 0
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create AgentRequest failed: {:?}",
        resp.errors
    );
}

async fn terminalize_request(
    node: &EmbeddedNode,
    request_id: &str,
    agent_did: &str,
    lifecycle_state: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let agent_did = escape_graphql_string(agent_did);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
                    request_id: {{ _eq: "{request_id}" }},
                    agent_did: {{ _eq: "{agent_did}" }}
                }},
                input: {{
                    lifecycle_state: "{lifecycle_state}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "terminalize AgentRequest failed: {:?}",
        resp.errors
    );
}

async fn fetch_request(node: &EmbeddedNode, request_id: &str) -> Option<AgentRequestRow> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                request_id
                agent_did
                lifecycle_state
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        return None;
    }
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .and_then(|row| serde_json::from_value(row).ok())
}

async fn wait_for_request_lifecycle(
    node: &EmbeddedNode,
    request_id: &str,
    expected_lifecycle: RequestLifecycleState,
    timeout: Duration,
    label: &str,
) -> AgentRequestRow {
    let deadline = Instant::now() + timeout;
    let mut last: Option<AgentRequestRow> = None;
    loop {
        if let Some(row) = fetch_request(node, request_id).await {
            if row.lifecycle_state == Some(expected_lifecycle) {
                return row;
            }
            last = Some(row);
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for AgentRequest({request_id}) \
                 lifecycle_state={expected_lifecycle} on {label}; last={last:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_push_worker_still_converges_agent_request_over_p2p() {
    let admission = TestP2pAdmission::single_push_worker();
    let owner = test_p2p_db_with_admission("admission-p2p-owner", admission.clone()).await;
    let peer = test_p2p_db_with_admission("admission-p2p-peer", admission).await;

    install_one_way_replicator(owner.node.as_ref(), peer.node.as_ref(), &["AgentRequest"]).await;

    let request_id = "admission-p2p-single-worker-req";
    let session_id = "admission-p2p-single-worker-session";

    create_request(
        owner.node.as_ref(),
        request_id,
        session_id,
        OWNER_DID,
        "processing",
    )
    .await;

    let on_peer_processing = wait_for_request_lifecycle(
        peer.node.as_ref(),
        request_id,
        RequestLifecycleState::Processing,
        Duration::from_secs(45),
        "peer (intermediate, single push worker)",
    )
    .await;
    assert_eq!(on_peer_processing.agent_did.as_deref(), Some(OWNER_DID));

    terminalize_request(owner.node.as_ref(), request_id, OWNER_DID, "completed").await;

    let on_peer_terminal = wait_for_request_lifecycle(
        peer.node.as_ref(),
        request_id,
        RequestLifecycleState::Completed,
        Duration::from_secs(45),
        "peer (terminal, single push worker)",
    )
    .await;
    assert_eq!(on_peer_terminal.agent_did.as_deref(), Some(OWNER_DID));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_push_worker_delivers_multi_wave_updates() {
    let admission = TestP2pAdmission::single_push_worker();
    let owner = test_p2p_db_with_admission("admission-p2p-multi-owner", admission.clone()).await;
    let peer = test_p2p_db_with_admission("admission-p2p-multi-peer", admission).await;

    install_one_way_replicator(owner.node.as_ref(), peer.node.as_ref(), &["AgentRequest"]).await;

    for idx in 0..3 {
        let request_id = format!("admission-p2p-multi-wave-{idx}");
        let session_id = format!("admission-p2p-multi-session-{idx}");
        create_request(
            owner.node.as_ref(),
            &request_id,
            &session_id,
            OWNER_DID,
            "completed",
        )
        .await;
        let on_peer = wait_for_request_lifecycle(
            peer.node.as_ref(),
            &request_id,
            RequestLifecycleState::Completed,
            Duration::from_secs(45),
            &format!("peer wave {idx}"),
        )
        .await;
        assert_eq!(on_peer.agent_did.as_deref(), Some(OWNER_DID));
    }
}
