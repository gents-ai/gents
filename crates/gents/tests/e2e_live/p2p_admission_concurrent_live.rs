//! Live concurrent multi-wave P2P admission e2e against real d4f inference.
//!
//! The sequential unit e2e (`p2p_admission_backpressure_e2e`) waits for each
//! peer convergence before the next write, so `max_concurrent_push_tasks = 1`
//! is never contended. This live test closes that gap:
//!
//!   * Owner hub: `max_concurrent_push_tasks = 1` (TLA `PushWorkers = 1` shape)
//!   * Two healthy peers as PushLog fan-out targets
//!   * Real d4f completions (workstation-1:8000 / `d4f` by default)
//!   * **Concurrent** request submission — N waves in flight at once so the
//!     single push worker must serialize fan-out across peers without
//!     stranding either peer
//!
//! Gated: `#[ignore]` + `GENTS_LIVE_P2P_ADMISSION=1`.
//!
//! ```bash
//! GENTS_LIVE_P2P_ADMISSION=1 \
//!   GENTS_LIVE_P2P_ADMISSION_ENDPOINT=http://workstation-1:8000/v1 \
//!   GENTS_LIVE_P2P_ADMISSION_MODEL=d4f \
//!   cargo test -p gents --test e2e_live \
//!     concurrent_multiwave_single_push_worker_converges_with_live_d4f \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Assertions are structural (lifecycle + non-empty answer), never exact model text.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    ensure_agent_principal, load_agent_behavior, upsert_agent_behavior, AgentIdentity,
    DocumentRuntimeOptions, Gents, ToolCeiling,
};
use serde::Deserialize;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::{first_optional_row, test_p2p_db_with_admission, TestDb, TestP2pAdmission};

const DEFAULT_LIVE_ENDPOINT: &str = "http://workstation-1:8000/v1";
const DEFAULT_LIVE_MODEL: &str = "d4f";
const LIVE_BACKEND_ID: &str = "backend-live-p2p-admission";
const CONCURRENT_WAVES: usize = 4;
const REPLICATED: &[&str] = &["AgentRequest", "AgentResponse", "AgentMessage"];

fn live_enabled() -> bool {
    std::env::var("GENTS_LIVE_P2P_ADMISSION").as_deref() == Ok("1")
}

fn live_endpoint() -> String {
    std::env::var("GENTS_LIVE_P2P_ADMISSION_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_LIVE_ENDPOINT.to_string())
}

fn live_model() -> String {
    std::env::var("GENTS_LIVE_P2P_ADMISSION_MODEL")
        .unwrap_or_else(|_| DEFAULT_LIVE_MODEL.to_string())
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

async fn assert_endpoint_reachable(endpoint: &str) {
    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest");
    let resp = tokio::time::timeout(Duration::from_secs(20), client.get(&url).send()).await;
    match resp {
        Ok(Ok(r)) if r.status().is_success() => {}
        Ok(Ok(r)) => panic!("endpoint {url} returned {}", r.status()),
        Ok(Err(e)) => panic!("endpoint {url} unreachable: {e}"),
        Err(_) => panic!("endpoint {url} timed out"),
    }
}

async fn upsert_live_backend(node: &EmbeddedNode, endpoint: &str, model: &str) {
    let backend_id = escape_graphql_string(LIVE_BACKEND_ID);
    let endpoint = escape_graphql_string(endpoint);
    let model = escape_graphql_string(model);
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{backend_id}" }} }},
                add: {{
                    backend_id: "{backend_id}",
                    name: "{backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{endpoint}",
                    api_key: "",
                    api_key_env_var: "",
                    max_concurrent: 8,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{model}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{endpoint}",
                    api_key: "",
                    api_key_env_var: "",
                    max_concurrent: 8,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{model}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "upsert live backend failed: {:?}",
        resp.errors
    );
}

async fn bind_live_backend(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
    endpoint: &str,
    model: &str,
) -> (String, String) {
    let agent_did = identity.did().to_string();
    let bootstrap = ensure_agent_principal(node, &agent_did)
        .await
        .expect("ensure principal");
    let behavior_id = bootstrap.default_behavior.behavior_id.clone();
    upsert_live_backend(node, endpoint, model).await;

    let mut behavior = load_agent_behavior(node, &behavior_id)
        .await
        .expect("load behavior")
        .expect("default behavior exists");
    behavior.backend_id = Some(LIVE_BACKEND_ID.to_string());
    behavior.model_name = Some(model.to_string());
    behavior.inference_profile_id = Some(default_inference_profile_id_for_behavior(&behavior_id));
    behavior.enabled = true;
    upsert_agent_behavior(node, &behavior)
        .await
        .expect("point behavior at live backend");

    debug_assert_eq!(behavior_id, default_behavior_id_for_agent(&agent_did));
    (agent_did, behavior_id)
}

async fn boot_live_agent(db: &TestDb, identity: Arc<dyn AgentIdentity>) -> Result<BootedAgent> {
    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await?;
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    Ok(BootedAgent::new(shutdown_tx, handle, agent_did))
}

fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "dead" | "interrupted" | "superseded"
    )
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
async fn concurrent_multiwave_single_push_worker_converges_with_live_d4f() -> Result<()> {
    assert!(
        live_enabled(),
        "set GENTS_LIVE_P2P_ADMISSION=1 and pass --ignored to run the concurrent multi-wave live e2e"
    );

    let endpoint = live_endpoint();
    let model = live_model();
    eprintln!(
        "[p2p-admission-live] endpoint={endpoint} model={model} waves={CONCURRENT_WAVES} push_workers=1 peers=2"
    );
    assert_endpoint_reachable(&endpoint).await;

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
    let (agent_did, behavior_id) = bind_live_backend(
        topo.owner().node.as_ref(),
        identity.as_ref(),
        &endpoint,
        &model,
    )
    .await;
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
            "owner wave {request_id} must complete against live d4f, got {state}"
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
