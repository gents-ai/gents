use std::time::{Duration, Instant};

use anyhow::Result;
use gents::defra_node::Node as EmbeddedNode;

use crate::support::builder_runtime::submit_request;
use crate::support::interrupt::BootedAgent;
use crate::support::live_inference::{
    bind_target, boot_live_agent, live_target, wait_for_request_terminal,
};
use crate::support::p2p_waits::wait_for_listen_addr;
use crate::support::{test_p2p_db_with_admission, TestDb, TestP2pAdmission};

const CONCURRENT_WAVES: usize = 4;

fn live_enabled() -> bool {
    std::env::var("GENTS_LIVE_P2P_ADMISSION").as_deref() == Ok("1")
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

/// P2P carries only discovery and enrollment: the owner dials each peer, and
/// no document collection replicates over the connection.
async fn connect_discovery_peer(owner: &EmbeddedNode, peer: &EmbeddedNode) {
    let peer_addr = wait_for_listen_addr(peer).await;
    // Transport administration runs as the Node's own identity.
    gents::store::identity::node_p2p(owner)
        .expect("owner p2p")
        .connect_peer(&peer_addr)
        .await
        .expect("connect owner → peer");
    wait_for_connected_peer(owner).await;
    wait_for_connected_peer(peer).await;
}

struct LiveTopologyGuard {
    agent: Option<BootedAgent>,
    owner: Option<TestDb>,
    peer_a: Option<TestDb>,
    peer_b: Option<TestDb>,
    shut_down: bool,
    runtime: gents::runtime::drop_cleanup::DropCleanup,
}

impl LiveTopologyGuard {
    fn new(owner: TestDb, peer_a: TestDb, peer_b: TestDb) -> Self {
        Self {
            agent: None,
            owner: Some(owner),
            peer_a: Some(peer_a),
            peer_b: Some(peer_b),
            shut_down: false,
            runtime: gents::runtime::drop_cleanup::DropCleanup::current(),
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
        let nodes: Vec<_> = [self.owner.take(), self.peer_a.take(), self.peer_b.take()]
            .into_iter()
            .flatten()
            .map(|db| db.node.clone())
            .collect();
        self.runtime.spawn(async move {
            for node in nodes {
                node.shutdown().await;
            }
        });
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

    connect_discovery_peer(&topo.owner().node, &topo.peer_a().node).await;
    connect_discovery_peer(&topo.owner().node, &topo.peer_b().node).await;

    let actors = bind_target(topo.owner(), &target).await;
    let node_did = actors.node_did().to_owned();
    let agent = boot_live_agent(topo.owner(), topo.owner().node_identity.clone()).await?;
    topo.set_agent(agent);
    eprintln!("[p2p-admission-live] owner ready did={node_did}");

    let wave_ids: Vec<String> = (0..CONCURRENT_WAVES)
        .map(|i| format!("p2p-adm-live-wave-{i}"))
        .collect();
    let session_ids: Vec<String> = (0..CONCURRENT_WAVES)
        .map(|i| format!("p2p-adm-live-session-{i}"))
        .collect();

    let submit_start = Instant::now();
    for (i, request_id) in wave_ids.iter().enumerate() {
        submit_request(
            &actors,
            request_id,
            &session_ids[i],
            "interactive",
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
        let state =
            wait_for_request_terminal(&actors.fixture.access, request_id, owner_deadline).await;
        assert_eq!(
            state, "completed",
            "owner wave {request_id} must complete against target {}, got {state}",
            target.name
        );
        eprintln!("[p2p-admission-live] owner terminal {request_id}={state}");
    }

    for db in [topo.peer_a(), topo.peer_b()] {
        assert!(
            !db.node
                .p2p()
                .expect("peer p2p")
                .connected_peers()
                .await
                .expect("connected peers")
                .is_empty(),
            "discovery peers stay connected through the concurrent waves"
        );
    }

    topo.shutdown().await;
    eprintln!("[p2p-admission-live] PASS concurrent multi-wave under single push worker with discovery peers");
    Ok(())
}
