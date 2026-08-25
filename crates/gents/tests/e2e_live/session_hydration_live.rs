//! Live two-node session hydration against real inference.
//!
//! This test creates requester-owned history on a runtime before its fresh
//! client exists. It then installs only the `SessionHydrationRequest` control
//! route, proving that transcript arrival comes from the hydration reconciler's
//! peer-targeted document push rather than standing transcript replication.
//!
//! Gated: `#[ignore]` + `GENTS_LIVE_SESSION_HYDRATION=1`.
//!
//! ```bash
//! GENTS_LIVE_SESSION_HYDRATION=1 \
//!   GENTS_LIVE_SESSION_HYDRATION_ENDPOINT=http://100.87.27.25:8000/v1 \
//!   GENTS_LIVE_SESSION_HYDRATION_MODEL=GLM-5.2 \
//!   cargo test -p gents --features live-e2e --test e2e_live \
//!     live_session_hydration_replays_history_to_a_fresh_client \
//!     -- --ignored --nocapture --test-threads=1
//! ```

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use gents::agent::p2p_reconcile::session_hydration::{
    observe_hydration_progress, ClientHydrationPhase, ClientHydrationProgress,
    HYDRATION_COLLECTIONS,
};
use gents::agent::p2p_reconcile::{
    resolve_template, resolve_template_filters, run_session_hydration_reconciler, PairingDirection,
    CLIENT_COLLECTIONS, CLIENT_TEMPLATE, CLIENT_TO_RUNTIME_COLLECTIONS,
};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    ensure_agent_principal, load_agent_behavior, upsert_agent_behavior, AgentIdentity,
    DocumentRuntimeOptions, Gents, ToolCeiling,
};
use gents_protocol::network_token::{derive_membership_key, MembershipRecord, NetworkRecord};
use serde::Deserialize;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{
    create_runtime_request_for_requester, wait_for_runtime_ready, BootedAgent,
};
use crate::support::p2p_waits::wait_for_listen_addr;
use crate::support::{first_optional_row, test_p2p_db, TestDb};

const DEFAULT_LIVE_ENDPOINT: &str = "http://100.87.27.25:8000/v1";
const DEFAULT_LIVE_MODEL: &str = "GLM-5.2";
const LIVE_BACKEND_ID: &str = "backend-live-session-hydration";
const NETWORK_ID: &str = "network-live-session-hydration";
const REQUEST_ID: &str = "request-live-session-hydration";
const SESSION_ID: &str = "session-live-session-hydration";
const CONTROL_COLLECTION: &str = "SessionHydrationRequest";

fn live_enabled() -> bool {
    std::env::var("GENTS_LIVE_SESSION_HYDRATION").as_deref() == Ok("1")
}

fn live_endpoint() -> String {
    std::env::var("GENTS_LIVE_SESSION_HYDRATION_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_LIVE_ENDPOINT.to_string())
}

fn live_model() -> String {
    std::env::var("GENTS_LIVE_SESSION_HYDRATION_MODEL")
        .unwrap_or_else(|_| DEFAULT_LIVE_MODEL.to_string())
}

fn bs58_sig(signature: &[u8]) -> String {
    bs58::encode(signature).into_string()
}

async fn assert_endpoint_reachable(endpoint: &str) {
    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client");
    match tokio::time::timeout(Duration::from_secs(20), client.get(&url).send()).await {
        Ok(Ok(response)) if response.status().is_success() => {}
        Ok(Ok(response)) => panic!("endpoint {url} returned {}", response.status()),
        Ok(Err(error)) => panic!("endpoint {url} unreachable: {error}"),
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
                    max_concurrent: 2,
                    max_queue_depth: 8,
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
                    max_concurrent: 2,
                    max_queue_depth: 8,
                    enabled: true,
                    models: ["{model}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert live backend failed: {:?}",
        response.errors
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
        .expect("ensure live principal");
    let behavior_id = bootstrap.default_behavior.behavior_id.clone();
    upsert_live_backend(node, endpoint, model).await;

    let mut behavior = load_agent_behavior(node, &behavior_id)
        .await
        .expect("load live behavior")
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

async fn wait_for_terminal(node: &EmbeddedNode, request_id: &str) {
    let request_id = escape_graphql_string(request_id);
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut last = String::from("<missing>");
    loop {
        let query = format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                lifecycle_state
            }} }}"#
        );
        #[derive(Deserialize)]
        struct Row {
            lifecycle_state: Option<String>,
        }
        let response = node.execute(&query).await;
        if let Some(row) = first_optional_row::<Row>(&response, "AgentRequest") {
            last = row.lifecycle_state.unwrap_or_default();
            if matches!(
                last.as_str(),
                "completed" | "failed" | "dead" | "interrupted" | "superseded"
            ) {
                assert_eq!(last, "completed", "live inference did not complete");
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for live request; last={last}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn seed_verified_membership(
    node: &EmbeddedNode,
    admin: &dyn AgentIdentity,
    requester: &dyn AgentIdentity,
) {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut network = NetworkRecord {
        network_id: NETWORK_ID.to_string(),
        admin_did: admin.did().to_string(),
        display_name: "Live Session Hydration".to_string(),
        default_template: "network-control".to_string(),
        created_at: now.clone(),
        sig: Vec::new(),
    };
    network.sig = admin
        .sign(&network.signing_payload())
        .await
        .expect("sign live network");
    let mut membership = MembershipRecord {
        network_id: NETWORK_ID.to_string(),
        member_did: requester.did().to_string(),
        status: "active".to_string(),
        granted_at: now,
        revoked_at: String::new(),
        sig: Vec::new(),
    };
    membership.sig = admin
        .sign(&membership.signing_payload())
        .await
        .expect("sign live membership");

    let network_id = escape_graphql_string(&network.network_id);
    let admin_did = escape_graphql_string(&network.admin_did);
    let display_name = escape_graphql_string(&network.display_name);
    let default_template = escape_graphql_string(&network.default_template);
    let created_at = escape_graphql_string(&network.created_at);
    let network_sig = escape_graphql_string(&bs58_sig(&network.sig));
    let member_did = escape_graphql_string(&membership.member_did);
    let granted_at = escape_graphql_string(&membership.granted_at);
    let membership_key = escape_graphql_string(&derive_membership_key(
        &membership.network_id,
        &membership.member_did,
    ));
    let membership_sig = escape_graphql_string(&bs58_sig(&membership.sig));
    let mutation = format!(
        r#"mutation {{
            network: create_AgentNetwork(input: {{
                network_id: "{network_id}",
                admin_did: "{admin_did}",
                display_name: "{display_name}",
                default_template: "{default_template}",
                created_at: "{created_at}",
                admin_sig: "{network_sig}"
            }}) {{ _docID }}
            membership: create_NetworkMembership(input: {{
                membership_key: "{membership_key}",
                network_id: "{network_id}",
                member_did: "{member_did}",
                status: "active",
                granted_at: "{granted_at}",
                revoked_at: "",
                admin_sig: "{membership_sig}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed signed hydration membership failed: {:?}",
        response.errors
    );
}

async fn seed_pairing_applied(
    node: &EmbeddedNode,
    client_peer_id: &str,
    client_addr: &str,
    requester_did: &str,
    agent_did: &str,
) {
    let template = resolve_template(CLIENT_TEMPLATE).expect("client template");
    let filters = resolve_template_filters(
        template,
        PairingDirection::ClientToRuntime,
        requester_did,
        agent_did,
    );
    let filters = escape_graphql_string(&serde_json::to_string(&filters).expect("pairing filters"));
    let peer_id = escape_graphql_string(client_peer_id);
    let client_addr = escape_graphql_string(client_addr);
    let agent_did = escape_graphql_string(agent_did);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_PeerPairingDesired(input: {{
                peer_id: "{peer_id}",
                agent_did: "{agent_did}",
                collections: ["{CONTROL_COLLECTION}"],
                replicator_addresses: ["{client_addr}"],
                template: "{CLIENT_TEMPLATE}",
                source: "session-hydration-live",
                created_at: "{now}",
                updated_at: "{now}"
            }}) {{ _docID }}
            create_PeerPairingApplied(input: {{
                peer_id: "{peer_id}",
                collections: ["{CONTROL_COLLECTION}"],
                replicator_addresses: ["{client_addr}"],
                replicator_filter: "{filters}",
                created_at: "{now}",
                updated_at: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed PeerPairingApplied failed: {:?}",
        response.errors
    );
}

async fn seed_all_collection_witnesses(
    node: &EmbeddedNode,
    request_doc_id: &str,
    requester_did: &str,
    agent_did: &str,
) {
    let request_doc_id = escape_graphql_string(request_doc_id);
    let requester_did = escape_graphql_string(requester_did);
    let agent_did = escape_graphql_string(agent_did);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            tool: create_AgentToolCall(input: {{
                tool_call_key: "hydration-live-tool-key",
                request_id: "{REQUEST_ID}",
                request_doc_id: "{request_doc_id}",
                session_id: "{SESSION_ID}",
                agent_did: "{agent_did}",
                requester_did: "{requester_did}",
                message_sequence: 900,
                tool_name: "hydration_live_fixture",
                tool_call_id: "hydration-live-tool-call",
                args: "{{}}",
                result: "fixture",
                status: "completed",
                lifecycle_state: "completed",
                started_at: "{now}",
                completed_at: "{now}"
            }}) {{ _docID }}
            compact: create_CompactionEntry(input: {{
                compaction_key: "hydration-live-compaction",
                session_id: "{SESSION_ID}",
                agent_did: "{agent_did}",
                requester_did: "{requester_did}",
                request_id: "{REQUEST_ID}",
                request_doc_id: "{request_doc_id}",
                sequence: 900,
                summary: "deterministic hydration collection witness",
                files_read: "",
                files_modified: "",
                messages_compacted: 0,
                compacted_through_sequence: 0,
                original_tokens: 0,
                compacted_tokens: 0,
                created_at: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed hydration witnesses failed: {:?}",
        response.errors
    );
    let tool_doc_id = response
        .data
        .as_ref()
        .and_then(|data| data.get("tool"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|value| value.as_str())
        .expect("tool witness doc id");
    let tool_doc_id = escape_graphql_string(tool_doc_id);
    let mutation = format!(
        r#"mutation {{
            result: create_AgentToolResult(input: {{
                tool_call_doc_id: "{tool_doc_id}",
                agent_did: "{agent_did}",
                requester_did: "{requester_did}",
                session_id: "{SESSION_ID}",
                tool_name: "hydration_live_fixture",
                tool_input: "{{}}",
                output_text: "fixture",
                truncated: false,
                truncation_metadata: "",
                conversation_doc_id: "",
                created_at: "{now}",
                discarded_because_interrupted: false
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed linked hydration witnesses failed: {:?}",
        response.errors
    );
}

async fn install_control_only_route(
    server: &EmbeddedNode,
    client: &EmbeddedNode,
) -> (String, String) {
    let server_addr = wait_for_listen_addr(server).await;
    let client_addr = wait_for_listen_addr(client).await;
    let server_p2p = server.p2p().expect("server P2P");
    let client_p2p = client.p2p().expect("client P2P");
    let client_peer_id = client_p2p.local_peer_id().await.expect("client peer id");

    client_p2p
        .connect_peer(&server_addr)
        .await
        .expect("connect hydration client to server");
    let receiver_collections = CLIENT_COLLECTIONS
        .iter()
        .copied()
        .map(str::to_string)
        .collect::<Vec<_>>();
    client_p2p
        .add_collections(receiver_collections.clone())
        .await
        .expect("subscribe hydration receiver collections");
    server_p2p
        .add_collections(vec![CONTROL_COLLECTION.to_string()])
        .await
        .expect("subscribe server hydration control collection");

    // The fresh client has no transcript documents to push. Registering the
    // full set on this direction establishes receiver authorization without
    // creating a standing server-to-client transcript replay.
    client_p2p
        .add_replicator(
            CLIENT_TO_RUNTIME_COLLECTIONS
                .iter()
                .map(|collection| (*collection).to_string())
                .collect(),
            Some(&server_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("install client-to-server hydration route");
    server_p2p
        .add_replicator(
            vec![CONTROL_COLLECTION.to_string()],
            Some(&client_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("install server control-status return route");
    (client_peer_id, client_addr)
}

#[derive(Deserialize)]
struct DocIdRow {
    #[serde(rename = "_docID")]
    doc_id: Option<String>,
}

async fn hydration_document_ids(
    node: &EmbeddedNode,
    requester_did: &str,
    agent_did: &str,
) -> BTreeSet<(String, String)> {
    let requester_did = escape_graphql_string(requester_did);
    let agent_did = escape_graphql_string(agent_did);
    let scope = format!(
        "requester_did: {{ _eq: \"{requester_did}\" }}, agent_did: {{ _eq: \"{agent_did}\" }}, session_id: {{ _eq: \"{SESSION_ID}\" }}"
    );
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ {scope} }}) {{ _docID }}
            AgentResponse(filter: {{ {scope} }}) {{ _docID }}
            AgentMessage(filter: {{ {scope} }}) {{ _docID }}
            AgentToolCall(filter: {{ {scope} }}) {{ _docID }}
            AgentToolResult(filter: {{ {scope} }}) {{ _docID }}
            CompactionEntry(filter: {{ {scope} }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query hydration documents failed: {:?}",
        response.errors
    );
    let mut documents = BTreeSet::new();
    for collection in [
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "CompactionEntry",
    ] {
        for row in gents::graphql::rows::<DocIdRow>(&response, collection).expect("document rows") {
            if let Some(doc_id) = row.doc_id.filter(|value| !value.is_empty()) {
                documents.insert((collection.to_string(), doc_id));
            }
        }
    }
    documents
}

#[derive(Debug, Deserialize)]
struct HydrationStatusRow {
    status: Option<String>,
    status_detail: Option<String>,
    served_doc_count: Option<i64>,
}

async fn hydration_status(node: &EmbeddedNode, request_key: &str) -> Option<HydrationStatusRow> {
    let request_key = escape_graphql_string(request_key);
    let query = format!(
        r#"{{ SessionHydrationRequest(filter: {{ request_key: {{ _eq: "{request_key}" }} }}) {{
            status status_detail served_doc_count
        }} }}"#
    );
    let response = node.execute(&query).await;
    first_optional_row(&response, "SessionHydrationRequest")
}

async fn wait_for_hydration_served(
    node: &EmbeddedNode,
    request_key: &str,
    expected_count: usize,
) -> usize {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = String::from("<missing>");
    loop {
        if let Some(row) = hydration_status(node, request_key).await {
            let status = row.status.unwrap_or_default();
            last = format!(
                "status={status} count={:?} detail={:?}",
                row.served_doc_count, row.status_detail
            );
            if status == "served" {
                let count = row.served_doc_count.unwrap_or_default().max(0) as usize;
                assert_eq!(
                    count, expected_count,
                    "server selected unexpected document set"
                );
                return count;
            }
            assert_ne!(status, "rejected", "hydration rejected: {last}");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for hydration served; last={last}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn create_hydration_request(
    client: &EmbeddedNode,
    request_key: &str,
    requester_did: &str,
    agent_did: &str,
) {
    let request_key = escape_graphql_string(request_key);
    let requester_did = escape_graphql_string(requester_did);
    let agent_did = escape_graphql_string(agent_did);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_SessionHydrationRequest(input: {{
                request_key: "{request_key}",
                requester_did: "{requester_did}",
                agent_did: "{agent_did}",
                session_id: "{SESSION_ID}",
                created_at: "{now}",
                status: "pending",
                status_detail: "",
                served_doc_count: 0
            }}) {{ _docID }}
        }}"#
    );
    let response = client.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create hydration request failed: {:?}",
        response.errors
    );
}

async fn wait_for_exact_documents(
    node: &EmbeddedNode,
    requester_did: &str,
    agent_did: &str,
    expected: &BTreeSet<(String, String)>,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let last = hydration_document_ids(node, requester_did, agent_did).await;
        if &last == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for exact hydration set; expected={expected:?} last={last:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

struct LiveGuard {
    agent: Option<BootedAgent>,
    hydration_cancel: Option<tokio_util::sync::CancellationToken>,
    hydration_handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    server: Option<TestDb>,
    client: Option<TestDb>,
    shut_down: bool,
}

impl LiveGuard {
    fn new(server: TestDb) -> Self {
        Self {
            agent: None,
            hydration_cancel: None,
            hydration_handle: None,
            server: Some(server),
            client: None,
            shut_down: false,
        }
    }

    fn server(&self) -> &TestDb {
        self.server.as_ref().expect("server held")
    }

    fn client(&self) -> &TestDb {
        self.client.as_ref().expect("client held")
    }

    fn start_hydration_reconciler(&mut self, identity: Arc<dyn AgentIdentity>) {
        let node = self.server().node.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle = tokio::spawn(run_session_hydration_reconciler(
            node,
            identity,
            cancel.clone(),
        ));
        self.hydration_cancel = Some(cancel);
        self.hydration_handle = Some(handle);
    }

    async fn shutdown(mut self) {
        self.shut_down = true;
        if let Some(agent) = self.agent.take() {
            agent.shutdown().await;
        }
        if let Some(cancel) = self.hydration_cancel.take() {
            cancel.cancel();
        }
        if let Some(handle) = self.hydration_handle.take() {
            tokio::time::timeout(Duration::from_secs(15), handle)
                .await
                .expect("hydration reconciler should stop promptly")
                .expect("hydration reconciler task should join")
                .expect("hydration reconciler should return ok");
        }
        if let Some(server) = self.server.take() {
            server.node.shutdown().await;
        }
        if let Some(client) = self.client.take() {
            client.node.shutdown().await;
        }
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        if self.shut_down {
            return;
        }
        if let Some(agent) = self.agent.take() {
            drop(agent);
        }
        if let Some(cancel) = self.hydration_cancel.take() {
            cancel.cancel();
        }
        if let Some(handle) = self.hydration_handle.take() {
            handle.abort();
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let nodes = [self.server.take(), self.client.take()]
                .into_iter()
                .flatten()
                .map(|db| db.node.clone())
                .collect::<Vec<_>>();
            handle.spawn(async move {
                for node in nodes {
                    node.shutdown().await;
                }
            });
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SESSION_HYDRATION=1 and pass --ignored"]
async fn live_session_hydration_replays_history_to_a_fresh_client() -> Result<()> {
    assert!(
        live_enabled(),
        "set GENTS_LIVE_SESSION_HYDRATION=1 and pass --ignored to run live hydration"
    );
    let endpoint = live_endpoint();
    let model = live_model();
    eprintln!("[session-hydration-live] endpoint={endpoint} model={model}");
    assert_endpoint_reachable(&endpoint).await;

    let server = test_p2p_db("session-hydration-live-server").await;
    let mut topology = LiveGuard::new(server);
    let owner_identity: Arc<dyn AgentIdentity> =
        Arc::new(test_identity("session-hydration-live-owner"));
    let requester_identity = test_identity("session-hydration-live-requester");
    let requester_did = requester_identity.did().to_string();
    let (agent_did, behavior_id) = bind_live_backend(
        topology.server().node.as_ref(),
        owner_identity.as_ref(),
        &endpoint,
        &model,
    )
    .await;
    let agent = boot_live_agent(topology.server(), owner_identity.clone()).await?;
    topology.agent = Some(agent);

    let request_doc_id = create_runtime_request_for_requester(
        topology.server().node.as_ref(),
        &agent_did,
        &behavior_id,
        REQUEST_ID,
        SESSION_ID,
        &requester_did,
        "Reply with one short sentence confirming live session hydration history.",
    )
    .await;
    wait_for_terminal(topology.server().node.as_ref(), REQUEST_ID).await;
    topology
        .agent
        .take()
        .expect("live agent running")
        .shutdown()
        .await;
    seed_all_collection_witnesses(
        topology.server().node.as_ref(),
        &request_doc_id,
        &requester_did,
        &agent_did,
    )
    .await;
    seed_verified_membership(
        topology.server().node.as_ref(),
        owner_identity.as_ref(),
        &requester_identity,
    )
    .await;

    let expected =
        hydration_document_ids(topology.server().node.as_ref(), &requester_did, &agent_did).await;
    let represented_collections = expected
        .iter()
        .map(|(collection, _)| collection.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        represented_collections,
        HYDRATION_COLLECTIONS.iter().copied().collect(),
        "live history plus deterministic witnesses must cover all client-routable hydration collections"
    );

    topology.client = Some(test_p2p_db("session-hydration-live-client").await);
    assert!(
        hydration_document_ids(topology.client().node.as_ref(), &requester_did, &agent_did)
            .await
            .is_empty(),
        "fresh client must begin without transcript history"
    );
    let (client_peer_id, client_addr) = install_control_only_route(
        topology.server().node.as_ref(),
        topology.client().node.as_ref(),
    )
    .await;
    seed_pairing_applied(
        topology.server().node.as_ref(),
        &client_peer_id,
        &client_addr,
        &requester_did,
        &agent_did,
    )
    .await;
    topology.start_hydration_reconciler(owner_identity.clone());

    let request_key = format!("{client_peer_id}:{SESSION_ID}");
    create_hydration_request(
        topology.client().node.as_ref(),
        &request_key,
        &requester_did,
        &agent_did,
    )
    .await;
    let served = wait_for_hydration_served(
        topology.server().node.as_ref(),
        &request_key,
        expected.len(),
    )
    .await;
    wait_for_exact_documents(
        topology.client().node.as_ref(),
        &requester_did,
        &agent_did,
        &expected,
    )
    .await;
    let client_status = wait_for_hydration_served(
        topology.client().node.as_ref(),
        &request_key,
        expected.len(),
    )
    .await;
    assert_eq!(client_status, served);

    let progress = observe_hydration_progress(
        &ClientHydrationProgress::default(),
        SESSION_ID,
        &agent_did,
        expected.len(),
        Some(served),
        false,
    );
    assert_eq!(progress.phase, ClientHydrationPhase::Complete);
    eprintln!(
        "[session-hydration-live] PASS served={served} exact_docs={} collections={}",
        expected.len(),
        HYDRATION_COLLECTIONS.len()
    );

    topology.shutdown().await;
    Ok(())
}
