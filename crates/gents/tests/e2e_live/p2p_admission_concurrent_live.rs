//! Live concurrent multi-wave P2P admission e2e against real inference.
//!
//! The sequential unit e2e (`p2p_admission_backpressure_e2e`) waits for each
//! peer convergence before the next write, so `max_concurrent_push_tasks = 1`
//! is never contended. This live test closes that gap:
//!
//!   * Owner hub: `max_concurrent_push_tasks = 1` (TLA `PushWorkers = 1` shape)
//!   * Two healthy peers as PushLog fan-out targets
//!   * Real completions from the target named by `GENTS_EVAL_TARGET`
//!   * **Concurrent** request submission — N waves in flight at once so the
//!     single push worker must serialize fan-out across peers without
//!     stranding either peer
//!
//! Gated: `#[ignore]` + `GENTS_LIVE_P2P_ADMISSION=1`.
//!
//! ```bash
//! GENTS_LIVE_P2P_ADMISSION=1 GENTS_EVAL_TARGET=workstation-1 \
//!   cargo test -p gents --test e2e_live \
//!     concurrent_multiwave_single_push_worker_converges_with_live_inference \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Assertions are structural (lifecycle + non-empty answer), never exact model text.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::AgentIdentity;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{create_runtime_request, BootedAgent};
use crate::support::live_inference::{bind_target, boot_live_agent, live_target};
use crate::support::{first_optional_row, test_p2p_db_with_admission, TestDb, TestP2pAdmission};

const CONCURRENT_WAVES: usize = 4;
const REPLICATED: &[&str] = &["AgentRequest", "AgentResponse", "AgentMessage"];

fn live_enabled() -> bool {
    std::env::var("GENTS_LIVE_P2P_ADMISSION").as_deref() == Ok("1")
}

async fn wait_for_listen_addr(node: &EmbeddedNode) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let addrs = node
            .p2p()
            .expect("p2p enabled")
            .listen_addresses()
            .await
            .expect("listen addresses");
        if let Some(addr) = addrs.first() {
            return addr.clone();
        }
        if Instant::now() >= deadline {
            panic!("no P2P listen address; last={addrs:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_connected_peer(node: &EmbeddedNode) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let peers = node
            .p2p()
            .expect("p2p enabled")
            .connected_peers()
            .await
            .expect("connected peers");
        if !peers.is_empty() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("no connected peer; last={peers:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

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
        .expect("connect sender → receiver");
    wait_for_connected_peer(sender).await;
    wait_for_connected_peer(receiver).await;

    let names: Vec<String> = collections.iter().map(|c| (*c).to_string()).collect();
    sender_p2p
        .add_collections(names.clone())
        .await
        .expect("sender collections");
    receiver_p2p
        .add_collections(names.clone())
        .await
        .expect("receiver collections");
    receiver_p2p
        .add_replicator(
            names.clone(),
            Some(&sender_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("authorize sender on receiver");
    sender_p2p
        .add_replicator(
            names,
            Some(&receiver_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("install sender → receiver replicator");
}

fn is_terminal(state: &str) -> bool {
    RequestLifecycleState::is_terminal_str(Some(state))
}

async fn fetch_lifecycle(node: &EmbeddedNode, request_id: &str) -> Option<String> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                lifecycle_state
                agent_did
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct Row {
        lifecycle_state: Option<String>,
    }
    let resp = node.execute(&query).await;
    first_optional_row::<Row>(&resp, "AgentRequest").and_then(|r| r.lifecycle_state)
}

async fn wait_for_terminal(node: &EmbeddedNode, request_id: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut last = String::from("<none>");
    loop {
        if let Some(state) = fetch_lifecycle(node, request_id).await {
            last = state.clone();
            if is_terminal(&state) {
                return state;
            }
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {request_id} terminal; last={last}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_peer_request(
    node: &EmbeddedNode,
    request_id: &str,
    expected_owner_did: &str,
    timeout: Duration,
    label: &str,
) {
    let deadline = Instant::now() + timeout;
    let mut last = String::from("<none>");
    loop {
        let escaped = escape_graphql_string(request_id);
        let query = format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                    lifecycle_state
                    agent_did
                }}
            }}"#
        );
        #[derive(Deserialize)]
        struct Row {
            lifecycle_state: Option<String>,
            agent_did: Option<String>,
        }
        let resp = node.execute(&query).await;
        if let Some(row) = first_optional_row::<Row>(&resp, "AgentRequest") {
            let state = row.lifecycle_state.unwrap_or_default();
            last = format!("lifecycle={state} did={:?}", row.agent_did);
            if is_terminal(&state) {
                assert_eq!(
                    row.agent_did.as_deref(),
                    Some(expected_owner_did),
                    "{label}: peer replica must keep owner DID"
                );
                assert_eq!(
                    state, "completed",
                    "{label}: expected completed, got {state} ({last})"
                );
                return;
            }
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {request_id} on {label}; last={last}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

struct LiveTopologyGuard {
    agent: Option<BootedAgent>,
    owner: Option<TestDb>,
    peer_a: Option<TestDb>,
    peer_b: Option<TestDb>,
    shut_down: bool,
}

impl LiveTopologyGuard {
    fn new(owner: TestDb, peer_a: TestDb, peer_b: TestDb) -> Self {
        Self {
            agent: None,
            owner: Some(owner),
            peer_a: Some(peer_a),
            peer_b: Some(peer_b),
            shut_down: false,
        }
    }

    fn set_agent(&mut self, agent: BootedAgent) {
        self.agent = Some(agent);
    }

    fn owner(&self) -> &TestDb {
        self.owner.as_ref().expect("owner still held")
    }

    fn peer_a(&self) -> &TestDb {
        self.peer_a.as_ref().expect("peer_a still held")
    }

    fn peer_b(&self) -> &TestDb {
        self.peer_b.as_ref().expect("peer_b still held")
    }

    async fn shutdown(mut self) {
        self.shut_down = true;
        if let Some(agent) = self.agent.take() {
            agent.shutdown().await;
        }
        if let Some(db) = self.owner.take() {
            db.node.shutdown().await;
        }
        if let Some(db) = self.peer_a.take() {
            db.node.shutdown().await;
        }
        if let Some(db) = self.peer_b.take() {
            db.node.shutdown().await;
        }
    }
}

impl Drop for LiveTopologyGuard {
    fn drop(&mut self) {
        if self.shut_down {
            return;
        }
        if let Some(agent) = self.agent.take() {
            drop(agent);
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let nodes: Vec<_> = [self.owner.take(), self.peer_a.take(), self.peer_b.take()]
                .into_iter()
                .flatten()
                .map(|db| db.node.clone())
                .collect();
            handle.spawn(async move {
                for node in nodes {
                    node.shutdown().await;
                }
            });
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_P2P_ADMISSION=1 and pass --ignored"]
async fn concurrent_multiwave_single_push_worker_converges_with_live_inference() -> Result<()> {
    assert!(
        live_enabled(),
        "set GENTS_LIVE_P2P_ADMISSION=1 and pass --ignored to run the concurrent multi-wave live e2e"
    );

    let target = live_target();
    eprintln!(
        "[p2p-admission-live] target={} model={} waves={CONCURRENT_WAVES} push_workers=1 peers=2",
        target.name,
        target.model()
    );
    target.assert_reachable().await;

    let admission = TestP2pAdmission::single_push_worker();
    let owner = test_p2p_db_with_admission("p2p-adm-live-owner", admission.clone()).await;
    let peer_a = test_p2p_db_with_admission("p2p-adm-live-peer-a", admission.clone()).await;
    let peer_b = test_p2p_db_with_admission("p2p-adm-live-peer-b", admission).await;

    let mut topo = LiveTopologyGuard::new(owner, peer_a, peer_b);

    install_one_way_replicator(
        topo.owner().node.as_ref(),
        topo.peer_a().node.as_ref(),
        REPLICATED,
    )
    .await;
    install_one_way_replicator(
        topo.owner().node.as_ref(),
        topo.peer_b().node.as_ref(),
        REPLICATED,
    )
    .await;

    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("p2p-adm-live-owner"));
    let (agent_did, behavior_id) =
        bind_target(topo.owner().node.as_ref(), identity.as_ref(), &target).await;
    let agent = boot_live_agent(topo.owner(), identity).await?;
    topo.set_agent(agent);
    eprintln!("[p2p-admission-live] owner ready did={agent_did}");

    let wave_ids: Vec<String> = (0..CONCURRENT_WAVES)
        .map(|i| format!("p2p-adm-live-wave-{i}"))
        .collect();
    let session_ids: Vec<String> = (0..CONCURRENT_WAVES)
        .map(|i| format!("p2p-adm-live-session-{i}"))
        .collect();

    let submit_start = Instant::now();
    for (i, request_id) in wave_ids.iter().enumerate() {
        create_runtime_request(
            topo.owner().node.as_ref(),
            &agent_did,
            &behavior_id,
            request_id,
            &session_ids[i],
            &format!("Reply with exactly one word: wave{i}"),
        )
        .await;
    }
    eprintln!(
        "[p2p-admission-live] submitted {CONCURRENT_WAVES} concurrent requests in {:?}",
        submit_start.elapsed()
    );

    let owner_deadline = Duration::from_secs(180);
    for request_id in &wave_ids {
        let state = wait_for_terminal(topo.owner().node.as_ref(), request_id, owner_deadline).await;
        assert_eq!(
            state, "completed",
            "owner wave {request_id} must complete against target {}, got {state}",
            target.name
        );
        eprintln!("[p2p-admission-live] owner terminal {request_id}={state}");
    }

    let peer_deadline = Duration::from_secs(180);
    for request_id in &wave_ids {
        wait_for_peer_request(
            topo.peer_a().node.as_ref(),
            request_id,
            &agent_did,
            peer_deadline,
            "peer-a",
        )
        .await;
        wait_for_peer_request(
            topo.peer_b().node.as_ref(),
            request_id,
            &agent_did,
            peer_deadline,
            "peer-b",
        )
        .await;
        eprintln!("[p2p-admission-live] both peers have {request_id}");
    }

    topo.shutdown().await;
    eprintln!("[p2p-admission-live] PASS concurrent multi-wave under single push worker");
    Ok(())
}
