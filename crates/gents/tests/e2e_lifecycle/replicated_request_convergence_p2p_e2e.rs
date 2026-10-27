use std::sync::Arc;
use std::time::{Duration, Instant};

use gents::agent::p2p_reconcile::{
    equality_filter, EmbeddedRemoteP2pAdmin, PairingFilters, RemoteP2pAdmin,
};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{RequestLifecycle, TERMINAL_REDRIVE_CAP};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::support::p2p_waits::{wait_for_connected_peer, wait_for_listen_addr};
use crate::support::test_p2p_db;

const OWNER_DID: &str = "did:test:convergence-p2p-owner";
const PEER_DID: &str = "did:test:convergence-p2p-peer";
const AGENT_ID: &str = "convergence-p2p-behavior";

async fn install_one_way_replicator(
    sender: &Arc<EmbeddedNode>,
    receiver: &Arc<EmbeddedNode>,
    collections: &[&str],
    requester_scoped: bool,
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
    let sender_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(sender));
    let mut filters = PairingFilters::new();
    if requester_scoped {
        filters.insert(
            "AgentRequest".to_string(),
            equality_filter("requester_did", PEER_DID),
        );
    }
    sender_admin
        .add_replicator(&[receiver_addr], &collection_names, &filters)
        .await
        .expect("install sender to receiver replicator");
}

async fn wait_for_acknowledged(owner: &Arc<EmbeddedNode>, peer: &EmbeddedNode, doc_ids: &[String]) {
    let peer_id = peer
        .p2p()
        .expect("peer p2p")
        .local_peer_id()
        .await
        .expect("peer id");
    let admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(owner));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let acknowledged = admin
            .documents_acknowledged(&peer_id, "AgentRequest", doc_ids)
            .await
            .expect("replication acknowledgement");
        if acknowledged.iter().all(|acknowledged| *acknowledged) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "peer never acknowledged {doc_ids:?}: {acknowledged:?} peer={peer_id} status={} replicators={:?}",
            push_backlog_status(owner).await,
            owner.p2p().unwrap().get_replicators().await
        );
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

async fn wait_for_push_enqueues(
    node: &EmbeddedNode,
    minimum_enqueued_total: u64,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = push_backlog_status(node).await;
        let backlog = &status["push_backlog"];
        if backlog["enqueued_total"].as_u64().unwrap_or(0) >= minimum_enqueued_total {
            return status;
        }
        if Instant::now() >= deadline {
            panic!(
                "push backlog never reached enqueued_total={minimum_enqueued_total}; last={status}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn create_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    lifecycle_state: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let node_did = escape_graphql_string(node_did);
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
                purpose: "normal",
                node_did: "{node_did}",
                requester_did: "{PEER_DID}",
                agent_id: "{AGENT_ID}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "convergence p2p e2e",
                lifecycle_state: "{lifecycle_state}",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 3,
                request_hop: 0
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
    node_did: &str,
    lifecycle_state: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let node_did = escape_graphql_string(node_did);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
                    request_id: {{ _eq: "{request_id}" }},
                    node_did: {{ _eq: "{node_did}" }}
                }},
                input: {{
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

async fn fetch_request(node: &EmbeddedNode, request_id: &str) -> Option<AgentRequestRow> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                _docID
                request_id
                node_did
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
async fn p2p_owner_terminal_converges_and_redrive_stays_stable() {
    let owner = test_p2p_db("convergence-p2p-owner-live").await;
    let peer = test_p2p_db("convergence-p2p-peer-live").await;

    // The pinned DefraDB records no acknowledgement for a filtered
    // replicator's pushes, so acknowledgement is observed on a collection route.
    install_one_way_replicator(&owner.node, &peer.node, &["AgentRequest"], false).await;

    let request_id = "convergence-p2p-live-req";
    let session_id = "convergence-p2p-live-session";

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
        Duration::from_secs(30),
        "peer (intermediate)",
    )
    .await;
    assert_eq!(
        on_peer_processing.node_did.as_deref(),
        Some(OWNER_DID),
        "peer replica must retain the owner's DID (peer is passive)"
    );
    assert_ne!(on_peer_processing.node_did.as_deref(), Some(PEER_DID));

    terminalize_request(owner.node.as_ref(), request_id, OWNER_DID, "completed").await;

    let on_owner = wait_for_request_lifecycle(
        owner.node.as_ref(),
        request_id,
        RequestLifecycleState::Completed,
        Duration::from_secs(5),
        "owner",
    )
    .await;

    let on_peer_terminal = wait_for_request_lifecycle(
        peer.node.as_ref(),
        request_id,
        RequestLifecycleState::Completed,
        Duration::from_secs(30),
        "peer (terminal, first delivery)",
    )
    .await;
    assert_eq!(on_peer_terminal.node_did.as_deref(), Some(OWNER_DID));
    assert_eq!(
        on_peer_terminal.lifecycle_state, on_owner.lifecycle_state,
        "peer must match owner terminal exactly"
    );

    let doc_id = on_owner.doc_id.clone().expect("owner doc id");
    wait_for_acknowledged(
        &owner.node,
        peer.node.as_ref(),
        std::slice::from_ref(&doc_id),
    )
    .await;

    let first = RequestLifecycle::redrive_terminal_convergence(&owner.node, OWNER_DID)
        .await
        .expect("redrive");
    assert_eq!(
        (first.acknowledged, first.reasserted),
        (1, 0),
        "an acknowledged terminal request is retired, never re-asserted; report={first:?}"
    );
    let retired = RequestLifecycle::redrive_terminal_convergence(&owner.node, OWNER_DID)
        .await
        .expect("redrive after retirement");
    assert!(
        retired.is_noop(),
        "a retired request leaves the re-drive budget; report={retired:?}"
    );

    let final_peer = fetch_request(peer.node.as_ref(), request_id)
        .await
        .expect("peer still has the request");
    assert_eq!(
        final_peer.lifecycle_state,
        Some(RequestLifecycleState::Completed)
    );
    assert_eq!(final_peer.node_did.as_deref(), Some(OWNER_DID));

    owner.node.shutdown().await;
    peer.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2p_full_replay_converges_after_offline_peer_exhausts_redrive_cap() {
    let owner = test_p2p_db("convergence-p2p-owner-late").await;
    let peer = test_p2p_db("convergence-p2p-peer-late").await;

    let request_id = "convergence-p2p-late-req";
    let session_id = "convergence-p2p-late-session";

    create_request(
        owner.node.as_ref(),
        request_id,
        session_id,
        OWNER_DID,
        "completed",
    )
    .await;
    wait_for_request_lifecycle(
        owner.node.as_ref(),
        request_id,
        RequestLifecycleState::Completed,
        Duration::from_secs(5),
        "owner (pre-join)",
    )
    .await;

    let mut reasserted_total = 0usize;
    for _ in 0..TERMINAL_REDRIVE_CAP {
        let report = RequestLifecycle::redrive_terminal_convergence(&owner.node, OWNER_DID)
            .await
            .expect("redrive while peer unavailable");
        reasserted_total += report.reasserted;
    }
    assert_eq!(reasserted_total, TERMINAL_REDRIVE_CAP as usize);
    let exhausted = RequestLifecycle::redrive_terminal_convergence(&owner.node, OWNER_DID)
        .await
        .expect("redrive after persistent cap");
    assert!(exhausted.is_noop(), "persistent cap must be exhausted");

    install_one_way_replicator(&owner.node, &peer.node, &["AgentRequest"], true).await;

    let on_peer = wait_for_request_lifecycle(
        peer.node.as_ref(),
        request_id,
        RequestLifecycleState::Completed,
        Duration::from_secs(30),
        "peer (after full replay)",
    )
    .await;
    assert_eq!(on_peer.node_did.as_deref(), Some(OWNER_DID));
    let still_exhausted = RequestLifecycle::redrive_terminal_convergence(&owner.node, OWNER_DID)
        .await
        .expect("redrive remains exhausted after peer replay");
    assert!(still_exhausted.is_noop());

    owner.node.shutdown().await;
    peer.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2p_terminal_redrive_pushes_once_per_routed_request() {
    const REQUESTS: usize = 13;
    const FORMER_PAIRINGS: u64 = 16;

    let owner = test_p2p_db("convergence-p2p-wave-owner").await;
    let peer = test_p2p_db("convergence-p2p-wave-peer").await;
    install_one_way_replicator(&owner.node, &peer.node, &["AgentRequest"], true).await;

    let mut doc_ids = Vec::with_capacity(REQUESTS);
    for index in 0..REQUESTS {
        let request_id = format!("convergence-p2p-wave-{index:02}");
        let session_id = format!("convergence-p2p-wave-session-{index:02}");
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
            Duration::from_secs(30),
            "peer (wave seed)",
        )
        .await;
        doc_ids.push(on_peer.doc_id.expect("peer doc id"));
    }
    wait_for_acknowledged(&owner.node, peer.node.as_ref(), &doc_ids).await;

    let baseline = wait_for_push_backlog_idle(owner.node.as_ref()).await;
    let enqueued_before = baseline["push_backlog"]["enqueued_total"]
        .as_u64()
        .expect("enqueued_total counter");
    let report = RequestLifecycle::redrive_terminal_convergence(&owner.node, OWNER_DID)
        .await
        .expect("terminal redrive wave");
    assert_eq!(
        (report.acknowledged, report.reasserted),
        (REQUESTS, 0),
        "acknowledged terminal requests are retired, never re-asserted; report={report:?}"
    );

    let after =
        wait_for_push_enqueues(owner.node.as_ref(), enqueued_before + REQUESTS as u64).await;
    let enqueued_after = after["push_backlog"]["enqueued_total"]
        .as_u64()
        .expect("enqueued_total counter");
    let measured_pushes = enqueued_after - enqueued_before;
    assert_eq!(
        measured_pushes, REQUESTS as u64,
        "one request-party route must enqueue one outbound push per document the wave writes"
    );

    let former_pushes = REQUESTS as u64 * FORMER_PAIRINGS;
    assert!(
        former_pushes / measured_pushes >= 5,
        "request wave-volume reduction must be at least 5x; former={former_pushes}, current={measured_pushes}"
    );

    owner.node.shutdown().await;
    peer.node.shutdown().await;
}
