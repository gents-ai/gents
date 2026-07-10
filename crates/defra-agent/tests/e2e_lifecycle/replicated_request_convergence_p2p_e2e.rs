//! Multi-node e2e for #664 `TerminalConverges` (owner re-drive + P2P apply).
//!
//! Single-node conformance (`conformance/replicated_request_convergence.rs`)
//! fences the owner half of `EmitTerminalDelta` (scope, terminal-only, cap,
//! DID guards). This file closes the distributed half the review called out:
//! a real second node must observe the owner's terminal state over P2P, and
//! the owner re-drive must re-push a higher-priority same-value delta without
//! forking the peer's view.
//!
//! Topology (mirrors `event_trigger_p2p_e2e` / R5 cross-deployment plumbing):
//!   - Owner: writes + terminalizes + re-drives (no full agent boot required —
//!     re-drive is a pure lifecycle seam).
//!   - Peer: passive replica (different DID); only applies owner deltas.
//!
//! We deliberately do **not** fault-inject a dropped PushLog (no DefraDB hook
//! for that yet). The load-bearing checks are:
//!   1. intermediate `processing` replicates (the #661 peer-visible shape),
//!   2. owner terminal update converges on the peer,
//!   3. owner re-drive re-asserts and the peer stays on the same terminal
//!      (LWW higher-priority same-value write does not regress or fork).
//!
//! A second scenario exhausts the durable re-drive budget while the peer is
//! absent, then proves a full replicator replay repairs the peer without an
//! unbounded stream of same-value request writes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use defra_agent::agent::p2p_reconcile::{
    EmbeddedRemoteP2pAdmin, FilterPredicate, PairingFilters, RemoteP2pAdmin,
};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{RequestLifecycle, TERMINAL_REDRIVE_CAP};
use serde::Deserialize;

use crate::support::test_p2p_db;

const OWNER_DID: &str = "did:defra-agent:convergence-p2p-owner";
const PEER_DID: &str = "did:defra-agent:convergence-p2p-peer";
const BEHAVIOR_ID: &str = "convergence-p2p-behavior";

#[derive(Debug, Clone, Deserialize)]
struct RequestRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    agent_did: String,
    status: String,
    lifecycle_state: String,
}

// --- Two-node P2P plumbing (same shape as event_trigger_p2p_e2e / R5) ---

async fn install_one_way_replicator(
    sender: &Arc<EmbeddedNode>,
    receiver: &Arc<EmbeddedNode>,
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
    // DefraDB needs both the sender-side push target and the receiver-side
    // authorization record. Data-flow under test remains sender -> receiver.
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
    let sender_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(sender));
    let mut filters = PairingFilters::new();
    filters.insert(
        "AgentRequest".to_string(),
        FilterPredicate {
            field: "requester_did".to_string(),
            value: PEER_DID.to_string(),
        },
    );
    sender_admin
        .add_replicator(&[receiver_addr], &collection_names, &filters)
        .await
        .expect("install sender to receiver replicator");
}

async fn wait_for_listen_addr(node: &EmbeddedNode) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let addrs = node
            .p2p()
            .expect("p2p should be enabled")
            .listen_addresses()
            .await
            .expect("listen addresses");
        if let Some(addr) = addrs.first() {
            return addr.clone();
        }
        if Instant::now() >= deadline {
            panic!("node never exposed a P2P listen address; last_addrs={addrs:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_connected_peer(node: &EmbeddedNode) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let peers = node
            .p2p()
            .expect("p2p should be enabled")
            .connected_peers()
            .await
            .expect("connected peers");
        if !peers.is_empty() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("node never reported a connected peer; last_peers={peers:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn push_backlog_status(node: &EmbeddedNode) -> serde_json::Value {
    node.p2p()
        .expect("p2p should be enabled")
        .sync_status()
        .await
        .expect("p2p sync status")
}

async fn wait_for_push_backlog_idle(node: &EmbeddedNode) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = push_backlog_status(node).await;
        let backlog = &status["push_backlog"];
        if backlog["queued_items"].as_u64() == Some(0) && backlog["active_jobs"].as_u64() == Some(0)
        {
            return status;
        }
        if Instant::now() >= deadline {
            panic!("push backlog never became idle; last={status}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_push_enqueues_and_idle(
    node: &EmbeddedNode,
    minimum_enqueued_total: u64,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = push_backlog_status(node).await;
        let backlog = &status["push_backlog"];
        if backlog["enqueued_total"].as_u64().unwrap_or(0) >= minimum_enqueued_total
            && backlog["queued_items"].as_u64() == Some(0)
            && backlog["active_jobs"].as_u64() == Some(0)
        {
            return status;
        }
        if Instant::now() >= deadline {
            panic!(
                "push backlog never reached enqueued_total={minimum_enqueued_total} and idle; last={status}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// --- AgentRequest helpers ---

async fn create_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    status: &str,
    lifecycle_state: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let agent_did = escape_graphql_string(agent_did);
    let status = escape_graphql_string(status);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let created_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let terminal_fields = if matches!(lifecycle_state.as_str(), "completed" | "failed") {
        format!(", terminalized_at: \"{created_at}\", terminal_redrive_attempts: 0")
    } else {
        String::new()
    };
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                requester_did: "{PEER_DID}",
                behavior_id: "{BEHAVIOR_ID}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "convergence p2p e2e",
                status: "{status}",
                lifecycle_state: "{lifecycle_state}",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 3,
                subagent_depth: 0
                {terminal_fields}
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
    status: &str,
    lifecycle_state: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let agent_did = escape_graphql_string(agent_did);
    let status = escape_graphql_string(status);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
                    request_id: {{ _eq: "{request_id}" }},
                    agent_did: {{ _eq: "{agent_did}" }}
                }},
                input: {{
                    status: "{status}",
                    lifecycle_state: "{lifecycle_state}",
                    terminalized_at: "{terminalized_at}",
                    terminal_redrive_attempts: 0
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

async fn fetch_request(node: &EmbeddedNode, request_id: &str) -> Option<RequestRow> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                _docID
                agent_did
                status
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
    expected_lifecycle: &str,
    timeout: Duration,
    label: &str,
) -> RequestRow {
    let deadline = Instant::now() + timeout;
    let mut last: Option<RequestRow> = None;
    loop {
        if let Some(row) = fetch_request(node, request_id).await {
            if row.lifecycle_state == expected_lifecycle {
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

/// Live path: intermediate processing replicates, owner terminal converges on
/// the peer, then owner re-drive re-asserts without changing the peer's view.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2p_owner_terminal_converges_and_redrive_stays_stable() {
    let owner = test_p2p_db("convergence-p2p-owner-live").await;
    let peer = test_p2p_db("convergence-p2p-peer-live").await;

    install_one_way_replicator(&owner.node, &peer.node, &["AgentRequest"]).await;

    let request_id = "convergence-p2p-live-req";
    let session_id = "convergence-p2p-live-session";

    // 1. Owner writes a non-terminal request (the #661 intermediate shape).
    create_request(
        owner.node.as_ref(),
        request_id,
        session_id,
        OWNER_DID,
        "processing",
        "processing",
    )
    .await;

    let on_peer_processing = wait_for_request_lifecycle(
        peer.node.as_ref(),
        request_id,
        "processing",
        Duration::from_secs(30),
        "peer (intermediate)",
    )
    .await;
    assert_eq!(
        on_peer_processing.agent_did, OWNER_DID,
        "peer replica must retain the owner's DID (peer is passive)"
    );
    // Sanity: peer DID is not the owner — the replica is foreign on the peer node.
    assert_ne!(on_peer_processing.agent_did, PEER_DID);

    // 2. Owner terminalizes — the primary PushLog path under test.
    terminalize_request(
        owner.node.as_ref(),
        request_id,
        OWNER_DID,
        "completed",
        "completed",
    )
    .await;

    let on_owner = wait_for_request_lifecycle(
        owner.node.as_ref(),
        request_id,
        "completed",
        Duration::from_secs(5),
        "owner",
    )
    .await;
    assert_eq!(on_owner.status, "completed");

    let on_peer_terminal = wait_for_request_lifecycle(
        peer.node.as_ref(),
        request_id,
        "completed",
        Duration::from_secs(30),
        "peer (terminal, first delivery)",
    )
    .await;
    assert_eq!(on_peer_terminal.status, "completed");
    assert_eq!(on_peer_terminal.agent_did, OWNER_DID);
    assert_eq!(
        on_peer_terminal.lifecycle_state, on_owner.lifecycle_state,
        "peer must match owner terminal exactly"
    );

    // 3. Owner re-drive: same-value re-assert (higher-priority CRDT delta).
    //    Peer must remain at the owner terminal — no fork / no regression.
    let first = RequestLifecycle::redrive_terminal_convergence(owner.node.as_ref(), OWNER_DID)
        .await
        .expect("redrive");
    assert!(
        first.reasserted >= 1,
        "owner re-drive must re-assert at least the terminal row; report={first:?}"
    );

    // Give PushLog a moment, then confirm peer is still converged.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after_redrive = wait_for_request_lifecycle(
        peer.node.as_ref(),
        request_id,
        "completed",
        Duration::from_secs(15),
        "peer (after re-drive)",
    )
    .await;
    assert_eq!(after_redrive.status, "completed");
    assert_eq!(after_redrive.agent_did, OWNER_DID);
    assert_eq!(after_redrive.doc_id, on_peer_terminal.doc_id);

    // 4. Cap exhaust: remaining re-asserts drain the budget; peer stays put.
    let mut total_reasserted = first.reasserted;
    for _ in 0..TERMINAL_REDRIVE_CAP {
        let report = RequestLifecycle::redrive_terminal_convergence(owner.node.as_ref(), OWNER_DID)
            .await
            .expect("redrive drain");
        total_reasserted += report.reasserted;
        if report.reasserted == 0 {
            break;
        }
    }
    assert!(
        total_reasserted <= TERMINAL_REDRIVE_CAP as usize,
        "redrive must not exceed TERMINAL_REDRIVE_CAP={TERMINAL_REDRIVE_CAP}, got {total_reasserted}"
    );
    let exhausted = RequestLifecycle::redrive_terminal_convergence(owner.node.as_ref(), OWNER_DID)
        .await
        .expect("redrive after cap");
    assert_eq!(
        exhausted.reasserted, 0,
        "after CAP re-asserts the row must self-drop from the re-drive budget"
    );

    let final_peer = fetch_request(peer.node.as_ref(), request_id)
        .await
        .expect("peer still has the request");
    assert_eq!(final_peer.lifecycle_state, "completed");
    assert_eq!(final_peer.agent_did, OWNER_DID);

    owner.node.shutdown().await;
    peer.node.shutdown().await;
}

/// Recovery path: the peer stays unavailable beyond the persisted re-drive cap.
/// Installing the replicator then performs a full replay, which must converge
/// the peer even though the owner is no longer eligible for same-value re-drive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2p_full_replay_converges_after_offline_peer_exhausts_redrive_cap() {
    let owner = test_p2p_db("convergence-p2p-owner-late").await;
    let peer = test_p2p_db("convergence-p2p-peer-late").await;

    let request_id = "convergence-p2p-late-req";
    let session_id = "convergence-p2p-late-session";

    // Owner reaches terminal with no peer yet.
    create_request(
        owner.node.as_ref(),
        request_id,
        session_id,
        OWNER_DID,
        "completed",
        "completed",
    )
    .await;
    let on_owner = wait_for_request_lifecycle(
        owner.node.as_ref(),
        request_id,
        "completed",
        Duration::from_secs(5),
        "owner (pre-join)",
    )
    .await;
    assert_eq!(on_owner.status, "completed");

    // Spend the entire persisted budget while the peer is unavailable.
    let mut reasserted_total = 0usize;
    for _ in 0..TERMINAL_REDRIVE_CAP {
        let report = RequestLifecycle::redrive_terminal_convergence(owner.node.as_ref(), OWNER_DID)
            .await
            .expect("redrive while peer unavailable");
        reasserted_total += report.reasserted;
    }
    assert_eq!(reasserted_total, TERMINAL_REDRIVE_CAP as usize);
    let exhausted = RequestLifecycle::redrive_terminal_convergence(owner.node.as_ref(), OWNER_DID)
        .await
        .expect("redrive after persistent cap");
    assert!(exhausted.is_noop(), "persistent cap must be exhausted");

    // Peer recovery installs a full replicator replay. This path does not author
    // another AgentRequest delta and therefore does not grow request history.
    install_one_way_replicator(&owner.node, &peer.node, &["AgentRequest"]).await;

    let on_peer = wait_for_request_lifecycle(
        peer.node.as_ref(),
        request_id,
        "completed",
        Duration::from_secs(30),
        "peer (after full replay)",
    )
    .await;
    assert_eq!(on_peer.agent_did, OWNER_DID);
    assert_eq!(on_peer.status, "completed");
    let still_exhausted =
        RequestLifecycle::redrive_terminal_convergence(owner.node.as_ref(), OWNER_DID)
            .await
            .expect("redrive remains exhausted after peer replay");
    assert!(still_exhausted.is_noop());

    owner.node.shutdown().await;
    peer.node.shutdown().await;
}

/// #683 wave-volume measurement using the same 13-doc × 16-pairing incident
/// shape as the #1103 harness. One real filtered two-node route observes the
/// sender's outbound enqueue counter; the former fleet-wide predicate is the
/// 16-replicator baseline. Each re-driven request now produces one push to its
/// requester, so the measured reduction is 16x (well above the 5x gate).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2p_terminal_redrive_pushes_once_per_routed_request() {
    const REQUESTS: usize = 13;
    const FORMER_PAIRINGS: u64 = 16;

    let owner = test_p2p_db("convergence-p2p-wave-owner").await;
    let peer = test_p2p_db("convergence-p2p-wave-peer").await;
    install_one_way_replicator(&owner.node, &peer.node, &["AgentRequest"]).await;

    for index in 0..REQUESTS {
        let request_id = format!("convergence-p2p-wave-{index:02}");
        let session_id = format!("convergence-p2p-wave-session-{index:02}");
        create_request(
            owner.node.as_ref(),
            &request_id,
            &session_id,
            OWNER_DID,
            "completed",
            "completed",
        )
        .await;
        wait_for_request_lifecycle(
            peer.node.as_ref(),
            &request_id,
            "completed",
            Duration::from_secs(30),
            "peer (wave seed)",
        )
        .await;
    }

    let baseline = wait_for_push_backlog_idle(owner.node.as_ref()).await;
    let enqueued_before = baseline["push_backlog"]["enqueued_total"]
        .as_u64()
        .expect("enqueued_total counter");

    let report = RequestLifecycle::redrive_terminal_convergence(owner.node.as_ref(), OWNER_DID)
        .await
        .expect("terminal redrive wave");
    assert_eq!(report.reasserted, REQUESTS);

    let after =
        wait_for_push_enqueues_and_idle(owner.node.as_ref(), enqueued_before + REQUESTS as u64)
            .await;
    let enqueued_after = after["push_backlog"]["enqueued_total"]
        .as_u64()
        .expect("enqueued_total counter");
    let measured_pushes = enqueued_after - enqueued_before;
    assert_eq!(
        measured_pushes, REQUESTS as u64,
        "one request-party route must enqueue one outbound push per re-driven document"
    );

    let former_pushes = REQUESTS as u64 * FORMER_PAIRINGS;
    assert!(
        former_pushes / measured_pushes >= 5,
        "request wave-volume reduction must be at least 5x; former={former_pushes}, current={measured_pushes}"
    );

    owner.node.shutdown().await;
    peer.node.shutdown().await;
}
