use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use gents::defra_node::Node as EmbeddedNode;
use gents::{DocumentRuntimeOptions, Gents};
use gents_loop::tool::Tool;
use gents_p2p::p2p_reconcile::{
    EmbeddedRemoteP2pAdmin, GraphqlEnrollmentStore, PairingFilters, RemoteP2pAdmin,
    RemoteP2pAdminError,
};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_tools::builtin::p2p::P2pTool;
use gents_tools::surface::ToolCeiling;
use serde_json::{json, Value};

use crate::support::fixtures::bind_default_agent_backend;
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::SnapshotReader;
use crate::support::{install_managed_agent, load_managed_fixture_user, test_p2p_db, TestDb};
use gents_p2p::test_support::enrollment::{
    authorize_enrollment_generation, authorize_enrollment_peer, wait_for_peer_identity,
    EnrollmentGeneration,
};

const NETWORK_ID: &str = "net-app-collection-pairing";
const NETWORK_NAME: &str = "App Collection Pairing Net";
const PAIRING_INPUTS: &[&str] = &[
    "DataPlanePairingDesired",
    "PeerPairingApplied",
    "PeerPairingDesired",
    "NetworkAuthorizationRevision",
    "NetworkEnrollmentDecision",
    "Agent",
    "AgentContext",
    "Tools",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_invitation_delivers_request_without_http() {
    use gents_p2p::p2p_reconcile::enrollment_candidate::request_enrollment;
    use gents_p2p::p2p_reconcile::enrollment_offer::EnrollmentOfferIssuer;
    use gents_protocol::enrollment::EnrollmentDecisionKind;

    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    let host = test_p2p_db("invite-host").await;
    let guest = test_p2p_db("invite-guest").await;
    wait_for_peer_identity(&host.node).await;
    wait_for_peer_identity(&guest.node).await;
    gents_p2p::test_support::enrollment::signed_enrollment_offer(
        &host.node,
        NETWORK_ID,
        NETWORK_NAME,
        host.node_identity.as_ref(),
        "setup-challenge".to_owned(),
    )
    .await;

    let issuer = EnrollmentOfferIssuer::new(
        host.node.clone(),
        host.node_identity.clone(),
        gents_store::identity::node_p2p(&host.node).unwrap(),
        NETWORK_ID.to_owned(),
        host.node_identity.did().to_owned(),
        "client".to_owned(),
    );
    let invite = issuer.mint().await.unwrap();
    assert!(invite.token.len() <= gents_protocol::enrollment::MAX_LINKABLE_OFFER_CHARACTERS);
    let mut host_updates = host.node.subscribe(&[gents::defra_node::EventName::Update]);
    let mut guest_updates = guest
        .node
        .subscribe(&[gents::defra_node::EventName::Update]);
    let request = request_enrollment(&guest.node, guest.node_identity.as_ref(), &invite.token)
        .await
        .unwrap();
    let store = GraphqlEnrollmentStore::new(host.node.clone(), host.node_identity.clone());
    let delivered = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let projection = store.load_projection().await.unwrap();
            if projection
                .pending
                .iter()
                .any(|item| item.request.request_id == request.request_id)
            {
                break;
            }
            host_updates
                .recv()
                .await
                .expect("host update subscription closed");
        }
    })
    .await;
    if delivered.is_err() {
        panic!(
            "invitation request did not become pending: {:?}",
            store.load_projection().await.unwrap()
        );
    }
    let approval = store
        .decide_request(&request.request_id, EnrollmentDecisionKind::Approved)
        .await
        .unwrap();
    assert_eq!(approval.state, "approved");
    let request_id = escape_graphql_string(&request.request_id);
    let query = format!(
        r#"{{ NetworkEnrollmentDecision(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ decision }} }}"#
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let response = gents_store::graphql::graphql_with_transaction_retry(
                &guest.node,
                &query,
                "observe invitation approval",
            )
            .await
            .unwrap();
            let rows = gents_store::graphql::rows::<Value>(&response, "NetworkEnrollmentDecision")
                .unwrap();
            if rows
                .iter()
                .any(|row| row.get("decision").and_then(Value::as_str) == Some("approved"))
            {
                break;
            }
            guest_updates
                .recv()
                .await
                .expect("guest update subscription closed");
        }
    })
    .await
    .expect("approved invitation did not reach the guest");
}

async fn add_collection(node: &Arc<EmbeddedNode>, sdl: &str) {
    ConfigAccess::local(node.clone())
        .add_collection(sdl)
        .await
        .unwrap_or_else(|error| panic!("add_collection {sdl}: {error}"));
}

async fn register_change_proposed_schema(node: &Arc<EmbeddedNode>) {
    add_collection(
        node,
        r#"
        type ChangeProposed @branchable {
            external_id: String
            payload: String
            kind: String @index
        }
    "#,
    )
    .await;
}

async fn register_guarded_change_schema(node: &Arc<EmbeddedNode>) {
    add_collection(
        node,
        &format!(
            r#"type GuardedChange @policy(id: "{}", resource: "configuration") {{ text: String }}"#,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
        ),
    )
    .await;
}

async fn read(node: &Arc<EmbeddedNode>, query: &str) -> gents::defra_node::QueryResponse {
    gents_store::graphql::graphql_with_transaction_retry(node, query, "test.p2p_observation")
        .await
        .expect("P2P document access")
}

async fn write(node: &Arc<EmbeddedNode>, mutation: &str) -> gents::defra_node::QueryResponse {
    ConfigAccess::write_local_response(node, "test.p2p_fixture", mutation)
        .await
        .expect("P2P document access")
}

async fn fetch_pairing_applied(node: &Arc<EmbeddedNode>, peer_id: &str) -> Option<Vec<String>> {
    let escaped = escape_graphql_string(peer_id);
    let response = read(
        node,
        &format!(
            r#"{{
            PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                peer_id
                replicator_addresses
            }}
        }}"#
        ),
    )
    .await;
    if response.has_errors() {
        return None;
    }
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("PeerPairingApplied"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()?;
    let addresses = row
        .get("replicator_addresses")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(addresses)
}

async fn replicator_carries(node: &Arc<EmbeddedNode>, peer: &str, collection: &str) -> bool {
    let admin = EmbeddedRemoteP2pAdmin::new(node.clone());
    let Some(id) = admin
        .resolve_collection_id(collection)
        .await
        .expect("resolve collection id")
    else {
        return false;
    };
    admin
        .list_replicators()
        .await
        .expect("list replicators")
        .iter()
        .any(|replicator| {
            (replicator.id.as_deref() == Some(peer)
                || replicator
                    .address
                    .as_deref()
                    .is_some_and(|address| address.contains(peer)))
                && replicator.collections.iter().any(|token| token == &id)
        })
}

async fn assert_never_carried(node: &Arc<EmbeddedNode>, peer_id: &str, collection: &str) {
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut changes = node.subscribe_changes("PeerPairingApplied").await;
    loop {
        fetch_pairing_applied(node, peer_id)
            .await
            .expect("control pairing stays applied");
        assert!(
            !replicator_carries(node, peer_id, collection).await,
            "{collection} was carried toward {peer_id}"
        );
        if tokio::time::timeout_at(deadline, changes.recv())
            .await
            .is_err()
        {
            break;
        }
    }
    assert!(!replicator_carries(node, peer_id, collection).await);
}

async fn refuse_at_transport(node: &Arc<EmbeddedNode>, peer_address: &str, collection: &str) {
    let refused = EmbeddedRemoteP2pAdmin::new(node.clone())
        .add_replicator(
            &[peer_address.to_owned()],
            &[collection.to_owned()],
            &PairingFilters::default(),
        )
        .await;
    assert!(
        matches!(&refused, Err(RemoteP2pAdminError::RuntimeLocalCollection(name)) if name == collection),
        "{collection} must not reach the transport: {refused:?}"
    );
}

async fn assert_stays_local<F, Fut>(
    nodes: &[&Arc<EmbeddedNode>],
    collection: &str,
    local: F,
    violation: &str,
) where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut changes = Vec::new();
    for node in nodes {
        changes.push(node.subscribe_changes(collection).await);
    }
    loop {
        assert!(local().await, "{violation}");
        let notified =
            futures::future::select_all(changes.iter_mut().map(|change| Box::pin(change.recv())));
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            break;
        }
    }
    assert!(local().await, "{violation}");
}

async fn wait_until<F, Fut>(
    node: &Arc<EmbeddedNode>,
    collections: &[&str],
    what: &str,
    condition: F,
) where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut changes =
        gents_store::subscription_source::subscribe_collections(node, collections.iter().copied())
            .expect("observe collections");
    while !condition().await {
        tokio::time::timeout_at(deadline, changes.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .expect("document subscription closed");
    }
}

async fn wait_for_enrollment_route_applied(
    node: &Arc<EmbeddedNode>,
    peer_id: &str,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    let mut last = String::from("<none>");
    let mut changes = node.subscribe_changes("PeerPairingApplied").await;
    loop {
        if let Some(addresses) = fetch_pairing_applied(node, peer_id).await {
            last = format!("addresses={addresses:?}");
            if addresses.iter().any(|a| !a.trim().is_empty()) {
                return;
            }
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for enrollment PeerPairingApplied({peer_id}); last={last}");
        }
        tokio::time::timeout_at(deadline, changes.recv())
            .await
            .unwrap_or_else(|_| panic!("enrollment route observation deadline: {last}"));
    }
}

async fn wait_for_current_route_receipt(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    request_digest: &str,
    member_peer: &str,
    timeout: Duration,
) {
    let request_id = escape_graphql_string(request_id);
    let deadline = Instant::now() + timeout;
    let mut changes = node
        .subscribe_changes("NetworkEnrollmentRouteReceipt")
        .await;
    loop {
        let response = read(
            node,
            &format!(
                r#"{{ NetworkEnrollmentRouteReceipt(
                    filter: {{ request_id: {{ _eq: "{request_id}" }} }}
                ) {{
                    request_digest member_peer authorization_sequence
                    authorization_expires_at direction signer_did admin_sig
                }} }}"#
            ),
        )
        .await;
        let diagnostic = format!("data={:?} errors={:?}", response.data, response.errors);
        let current = response
            .data
            .as_ref()
            .and_then(|data| data.get("NetworkEnrollmentRouteReceipt"))
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                rows.len() == 1
                    && rows[0].get("request_digest").and_then(Value::as_str) == Some(request_digest)
                    && rows[0].get("member_peer").and_then(Value::as_str) == Some(member_peer)
                    && rows[0]
                        .get("authorization_sequence")
                        .and_then(Value::as_i64)
                        .is_some_and(|sequence| sequence > 0)
                    && rows[0]
                        .get("authorization_expires_at")
                        .and_then(Value::as_str)
                        .is_some_and(|expires_at| !expires_at.is_empty())
                    && rows[0].get("direction").and_then(Value::as_str) == Some("client_to_server")
                    && rows[0]
                        .get("signer_did")
                        .and_then(Value::as_str)
                        .is_some_and(|did| !did.is_empty())
                    && rows[0]
                        .get("admin_sig")
                        .and_then(Value::as_str)
                        .is_some_and(|signature| !signature.is_empty())
            });
        if current {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for directly delivered current route receipt: {diagnostic}");
        }
        tokio::time::timeout_at(deadline, changes.recv())
            .await
            .unwrap_or_else(|_| panic!("route receipt deadline: {diagnostic}"));
    }
}

async fn wait_for_revoked_route_teardown(
    server: &Arc<EmbeddedNode>,
    member: &Arc<EmbeddedNode>,
    request_id: &str,
    member_peer: &str,
    timeout: Duration,
) {
    let request_id = escape_graphql_string(request_id);
    let member_peer = escape_graphql_string(member_peer);
    let deadline = Instant::now() + timeout;
    let mut server_changes = gents_store::subscription_source::subscribe_collections(
        server,
        ["PeerPairingDesired", "PeerPairingApplied"],
    )
    .unwrap();
    let mut member_changes = member
        .subscribe_changes("NetworkAuthorizationRevision")
        .await;
    loop {
        let server_response = read(
            server,
            &format!(
                r#"{{
                    PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{member_peer}" }} }}) {{ peer_id }}
                    PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{member_peer}" }} }}) {{ peer_id }}
                }}"#
            ),
        )
        .await;
        let member_response = read(
            member,
            &format!(
                r#"{{ NetworkAuthorizationRevision(
                    filter: {{ request_id: {{ _eq: "{request_id}" }}, kind: {{ _eq: "revoked" }} }}
                ) {{ sequence kind signer_did admin_sig }} }}"#
            ),
        )
        .await;
        let empty = |response: &gents::defra_node::QueryResponse, field: &str| {
            !response.has_errors()
                && response
                    .data
                    .as_ref()
                    .and_then(|data| data.get(field))
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
        };
        let revoked = !member_response.has_errors()
            && member_response
                .data
                .as_ref()
                .and_then(|data| data.get("NetworkAuthorizationRevision"))
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.len() == 1
                        && rows[0].get("kind").and_then(Value::as_str) == Some("revoked")
                        && rows[0]
                            .get("sequence")
                            .and_then(Value::as_i64)
                            .is_some_and(|sequence| sequence > 1)
                        && rows[0]
                            .get("signer_did")
                            .and_then(Value::as_str)
                            .is_some_and(|did| !did.is_empty())
                        && rows[0]
                            .get("admin_sig")
                            .and_then(Value::as_str)
                            .is_some_and(|signature| !signature.is_empty())
                });
        if empty(&server_response, "PeerPairingDesired")
            && empty(&server_response, "PeerPairingApplied")
            && revoked
        {
            return;
        }
        let diagnostic = format!(
            "server_data={:?} server_errors={:?} member_data={:?} member_errors={:?}",
            server_response.data,
            server_response.errors,
            member_response.data,
            member_response.errors
        );
        if Instant::now() >= deadline {
            panic!("timed out waiting for signed revocation and route teardown: {diagnostic}");
        }
        tokio::time::timeout_at(deadline, async {
            tokio::select! {
                result = server_changes.recv() => { result.expect("route subscription closed"); }
                _ = member_changes.recv() => {}
            }
        })
        .await
        .unwrap_or_else(|_| panic!("route teardown deadline: {diagnostic}"));
    }
}

async fn write_change_proposed(node: &Arc<EmbeddedNode>, external_id: &str, kind: &str) -> String {
    let escaped_external_id = escape_graphql_string(external_id);
    let escaped_kind = escape_graphql_string(kind);
    let response = write(
        node,
        &format!(
            r#"mutation {{
            add_ChangeProposed(input: {{
                external_id: "{escaped_external_id}",
                payload: "{{}}",
                kind: "{escaped_kind}"
            }}) {{ _docID }}
        }}"#
        ),
    )
    .await;
    assert!(
        !response.has_errors(),
        "add_ChangeProposed failed: {:?}",
        response.errors
    );
    let data = response
        .data
        .as_ref()
        .expect("add_ChangeProposed response missing data");
    let field = data
        .get("add_ChangeProposed")
        .or_else(|| data.get("create_ChangeProposed"))
        .unwrap_or_else(|| panic!("add_/create_ChangeProposed key missing; data={data:?}"));
    field
        .get("_docID")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            field
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| panic!("ChangeProposed mutation returned no _docID: {field}"))
}

async fn query_change_proposed(node: &Arc<EmbeddedNode>) -> Vec<Value> {
    read(node, r#"{ ChangeProposed { external_id kind } }"#)
        .await
        .data
        .as_ref()
        .and_then(|data| data.get("ChangeProposed"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

async fn change_ids(node: &Arc<EmbeddedNode>) -> Vec<String> {
    query_change_proposed(node)
        .await
        .iter()
        .filter_map(|row| {
            row.get("external_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

async fn has_change(node: &Arc<EmbeddedNode>, external_id: &str) -> bool {
    change_ids(node).await.iter().any(|id| id == external_id)
}

async fn wait_for_change(node: &Arc<EmbeddedNode>, external_id: &str) {
    wait_until(
        node,
        &["ChangeProposed"],
        &format!("ChangeProposed {external_id}"),
        || async { has_change(node, external_id).await },
    )
    .await;
}

async fn write_change_note(node: &Arc<EmbeddedNode>, text: &str) {
    let text = escape_graphql_string(text);
    let response = write(
        node,
        &format!(r#"mutation {{ create_ChangeNote(input: {{ text: "{text}" }}) {{ _docID }} }}"#),
    )
    .await;
    assert!(
        !response.has_errors(),
        "create_ChangeNote failed: {:?}",
        response.errors
    );
}

async fn change_note_texts(node: &Arc<EmbeddedNode>) -> Vec<String> {
    let response = read(node, "{ ChangeNote { text } }").await;
    assert!(
        !response.has_errors(),
        "ChangeNote query failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("ChangeNote"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("text").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

async fn wait_for_carry(node: &Arc<EmbeddedNode>, peer: &str, collection: &str, carried: bool) {
    wait_until(
        node,
        &["PeerPairingApplied"],
        &format!("the route toward {peer} to carry {collection}: {carried}"),
        || async { replicator_carries(node, peer, collection).await == carried },
    )
    .await;
}

async fn p2p_call(tool: &P2pTool, value: Value) -> Result<Value, String> {
    Tool::call(
        tool,
        serde_json::from_value(value).expect("valid p2p parameters"),
    )
    .await
    .map(|output| serde_json::from_str(&output).expect("p2p replies with JSON"))
    .map_err(|error| error.to_string())
}

fn pairing_args(action: &str, peer: &str, did: &str, collections: &[&str]) -> Value {
    json!({"action":action,"peer_id":peer,"peer_did":did,"collections":collections})
}

fn pairing_tool(db: &TestDb, agent: Option<Arc<dyn NodeIdentity>>, mutate: bool) -> P2pTool {
    P2pTool::new(
        db.node.clone(),
        Some(db.node_identity.clone()),
        agent,
        mutate,
    )
}

struct PairingAgent {
    identity: Arc<dyn NodeIdentity>,
    binding: gents_protocol::agent_identity::AgentIdentityBinding,
    user: KeyIdentity,
    label: String,
}

impl PairingAgent {
    async fn configure(&self, db: &TestDb, p2p_mutations: bool, enabled: bool) {
        use gents_documents::{
            apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        use gents_store::collection::Collection;
        let runtime = db.node_identity.did();
        let tools_id = format!("{}-tools", self.label);
        let context_id = format!("{}-context", self.label);
        let documents = [
            (
                Collection::Tools,
                json!({"node_did": runtime, "tools_id": tools_id,
                    "built_ins": {"enable_p2p_tool": true, "enable_p2p_mutations": p2p_mutations}}),
            ),
            (
                Collection::AgentContext,
                json!({"node_did": runtime, "context_id": context_id, "tools_id": tools_id}),
            ),
            (
                Collection::Agent,
                json!({"node_did": runtime, "agent_did": self.binding.agent_did,
                    "creation_nonce": self.binding.creation_nonce, "display_name": self.label,
                    "inference_profile_id": format!("{}:inference", self.binding.agent_did),
                    "enabled": enabled, "context_id": context_id}),
            ),
        ];
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )
        .expect("render pairing Agent");
        db.client_access(&self.user)
            .transact_write("test.pairing_agent", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .expect("configure pairing Agent as the User");
    }
}

async fn pairing_agent(db: &TestDb, label: &str) -> PairingAgent {
    let user = load_managed_fixture_user(db).await;
    let access = db.client_access(&user);
    let (identity, binding) = install_managed_agent(db, &access, &user, label).await;
    let agent = PairingAgent {
        identity,
        binding,
        user,
        label: label.to_owned(),
    };
    agent.configure(db, true, true).await;
    agent
}

async fn pairing_status(tool: &P2pTool, peer: &str) -> Value {
    let listed = p2p_call(tool, json!({"action":"pairings_get","peer_id":peer}))
        .await
        .expect("pairings_get");
    listed["outcome"]["desired"][0].clone()
}

async fn wait_for_status(db: &TestDb, tool: &P2pTool, peer: &str, reason: Option<&str>) {
    wait_until(
        &db.node,
        PAIRING_INPUTS,
        &format!("the pairing toward {peer} to reach {reason:?}"),
        || async {
            let status = pairing_status(tool, peer).await;
            match reason {
                None => status["status"] == "active",
                Some(reason) => {
                    status["status"] == "inactive" && status["inactive_reason"] == reason
                }
            }
        },
    )
    .await;
}

async fn current_authorization(db: &TestDb, peer: &str) -> (String, u64, String) {
    let projection = GraphqlEnrollmentStore::new(db.node.clone(), db.node_identity.clone())
        .load_projection()
        .await
        .expect("enrollment projection");
    let active = projection
        .active
        .iter()
        .find(|entry| entry.request.candidate_peer == peer)
        .expect("peer is enrolled");
    (
        active.request.candidate_did.clone(),
        active.revision.sequence,
        active.revision.network_id.clone(),
    )
}

fn runtime_access(db: &TestDb) -> ConfigAccess {
    ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).expect("runtime DID"),
    )
}

async fn write_direct_overlay(
    db: &TestDb,
    agent: &PairingAgent,
    peer: &str,
    collection: &str,
    template: &str,
) {
    let (member, sequence, network) = current_authorization(db, peer).await;
    let nodes = runtime_access(db)
        .execute(&format!(
            r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
            escape_graphql_string(db.node_identity.did())
        ))
        .await
        .expect("read Node");
    let node_doc = nodes["data"]["Node"][0]["_docID"]
        .as_str()
        .expect("Node document")
        .to_owned();
    let create = format!(
        r#"mutation {{ create_DataPlanePairingDesired(input: {{peer_id: "{}", node_did: "{}", actor_did: "{}", collections: ["{}"], replicator_addresses: ["direct"], template: "{}", source: "engineer", enrollment_member_did: "{}", enrollment_sequence: {sequence}, enrollment_network_id: "{}"}}) {{ _docID }} }}"#,
        escape_graphql_string(peer),
        escape_graphql_string(db.node_identity.did()),
        escape_graphql_string(agent.identity.did()),
        escape_graphql_string(collection),
        escape_graphql_string(template),
        escape_graphql_string(&member),
        escape_graphql_string(&network),
    );
    ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.identity.did()).expect("agent DID"),
        agent.identity.as_ref(),
    )
    .expect("agent access")
    .transact_write("test.direct_overlay", |txn| {
        let create = &create;
        let node_doc = &node_doc;
        Box::pin(async move {
            let created = txn.execute(create).await?;
            let doc = gents_protocol::graphql::extract_mutation_doc_id(
                &created,
                "DataPlanePairingDesired",
            )?;
            txn.add_resource_relationship(
                "DataPlanePairingDesired",
                &doc,
                "parent",
                "nodes",
                node_doc,
            )
            .await?;
            Ok::<_, anyhow::Error>(())
        })
    })
    .await
    .expect("write a direct overlay as the Agent");
}

fn refused_or_empty(result: &anyhow::Result<Value>, field: &str) -> bool {
    !result.as_ref().is_ok_and(|value| {
        value["data"][field]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    })
}

async fn start_runtime(
    db: &TestDb,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let runtime = Gents::from_default_agent_documents(
        db.node.clone(),
        db.node_identity.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    (stop, tokio::spawn(runtime.run(receiver)))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_documents_replicate_only_after_each_node_opts_in() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");

    let db_a = test_p2p_db("app-collection-pairing-a").await;
    let db_b = test_p2p_db("app-collection-pairing-b").await;
    for db in [&db_a, &db_b] {
        register_change_proposed_schema(&db.node).await;
        add_collection(&db.node, "type ChangeNote { text: String }").await;
        register_guarded_change_schema(&db.node).await;
    }

    let identity_a = db_a.node_identity.clone();
    let identity_b = db_b.node_identity.clone();

    let mock_a = MockModelEndpoint::start("default").unwrap();
    let mock_b = MockModelEndpoint::start("default").unwrap();
    bind_default_agent_backend(&db_a, "backend-app-collection-a", mock_a.endpoint()).await;
    bind_default_agent_backend(&db_b, "backend-app-collection-b", mock_b.endpoint()).await;

    let opting_agent_a = pairing_agent(&db_a, "pairer-a").await.identity;
    let opting_agent_b = pairing_agent(&db_b, "pairer-b").await.identity;
    let tool_a = pairing_tool(&db_a, Some(opting_agent_a.clone()), true);
    let tool_b = pairing_tool(&db_b, Some(opting_agent_b), true);
    let other_agent = pairing_agent(&db_a, "sibling-a").await.identity;
    let other_agent_tool = pairing_tool(&db_a, Some(other_agent.clone()), true);

    let (stop_a, task_a) = start_runtime(&db_a).await;
    let (stop_b, task_b) = start_runtime(&db_b).await;

    let (peer_a, addr_a) = wait_for_peer_identity(&db_a.node).await;
    let (peer_b, addr_b) = wait_for_peer_identity(&db_b.node).await;

    let enrollment_a_to_b = authorize_enrollment_peer(
        db_a.node.clone(),
        NETWORK_ID,
        NETWORK_NAME,
        identity_a.clone(),
        identity_b.clone(),
        &peer_b,
        &addr_b,
    )
    .await;
    authorize_enrollment_peer(
        db_b.node.clone(),
        NETWORK_ID,
        NETWORK_NAME,
        identity_b.clone(),
        identity_a.clone(),
        &peer_a,
        &addr_a,
    )
    .await;
    wait_for_enrollment_route_applied(&db_a.node, &peer_b, Duration::from_secs(60)).await;
    wait_for_current_route_receipt(
        &db_b.node,
        &enrollment_a_to_b.request_id,
        &enrollment_a_to_b.request_digest,
        &peer_b,
        Duration::from_secs(60),
    )
    .await;
    wait_for_enrollment_route_applied(&db_b.node, &peer_a, Duration::from_secs(60)).await;

    write_change_proposed(&db_a.node, "before-opt-in", "signup").await;
    assert_never_carried(&db_a.node, &peer_b, "ChangeProposed").await;
    assert_never_carried(&db_b.node, &peer_a, "ChangeProposed").await;
    assert_stays_local(
        &[&db_b.node],
        "ChangeProposed",
        || async { query_change_proposed(&db_b.node).await.is_empty() },
        "enrollment alone replicated an application document",
    )
    .await;
    assert_eq!(query_change_proposed(&db_a.node).await.len(), 1);

    let pairing =
        |action, collections: &[&str]| pairing_args(action, &peer_b, identity_b.did(), collections);
    for (collection, refusal) in [
        ("OAuthCredential", "protocol, runtime or credential"),
        ("InferenceBackend", "protocol, runtime or credential"),
        ("AgentRequest", "protocol, runtime or credential"),
        ("PeerRegistry", "protocol, runtime or credential"),
        ("GuardedChange", "document access policy"),
        ("NoSuchCollection", "not registered"),
    ] {
        for action in ["pairings_preview", "pairings_apply"] {
            let error = p2p_call(&tool_a, pairing(action, &[collection]))
                .await
                .unwrap_err();
            assert!(error.contains(refusal), "{collection} {action}: {error}");
        }
    }
    let preview = p2p_call(
        &tool_a,
        pairing("pairings_preview", &["ChangeProposed", "ChangeNote"]),
    )
    .await
    .expect("preview an application collection");
    assert_eq!(
        preview["outcome"]["proposed"]["template"],
        "app-collections"
    );
    assert_eq!(
        preview["outcome"]["local_schema"]["ChangeNote"]["branchable"],
        false
    );
    let readonly = pairing_tool(&db_a, Some(opting_agent_a), false);
    let denied = p2p_call(&readonly, pairing("pairings_apply", &["ChangeProposed"]))
        .await
        .unwrap_err();
    assert!(denied.contains("mutations are disabled"), "{denied}");
    let no_agent = pairing_tool(&db_a, None, true);
    let refused = p2p_call(&no_agent, pairing("pairings_apply", &["ChangeProposed"]))
        .await
        .unwrap_err();
    assert!(
        refused.contains("Node identity must not stand in"),
        "{refused}"
    );
    let listed = p2p_call(&tool_a, json!({"action":"pairings_get","peer_id":peer_b}))
        .await
        .unwrap();
    assert_eq!(listed["outcome"]["desired"], json!([]));

    let applied = p2p_call(
        &tool_a,
        pairing("pairings_apply", &["ChangeProposed", "ChangeNote"]),
    )
    .await
    .expect("apply pairing");
    assert_eq!(applied["outcome"]["unchanged"], false);
    assert_eq!(applied["next_call"]["action"], "pairings_get");
    wait_for_status(&db_a, &tool_a, &peer_b, None).await;
    wait_for_change(&db_b.node, "before-opt-in").await;
    write_change_proposed(&db_a.node, "after-opt-in", "signup").await;
    write_change_note(&db_a.node, "non-branchable note").await;
    wait_for_change(&db_b.node, "after-opt-in").await;
    wait_until(
        &db_b.node,
        &["ChangeNote"],
        "the non-branchable note",
        || async {
            change_note_texts(&db_b.node)
                .await
                .iter()
                .any(|text| text == "non-branchable note")
        },
    )
    .await;
    assert!(
        replicator_carries(&db_a.node, &peer_b, "ChangeProposed").await,
        "the applied route must carry the application collection"
    );
    assert!(!replicator_carries(&db_a.node, &peer_b, "GuardedChange").await);
    let retried = p2p_call(
        &tool_a,
        pairing("pairings_apply", &["ChangeProposed", "ChangeNote"]),
    )
    .await
    .expect("retry apply pairing");
    assert_eq!(retried["outcome"]["unchanged"], true);

    for call in [
        pairing("pairings_apply", &["ChangeProposed"]),
        json!({"action":"pairings_revoke","peer_id":peer_b}),
    ] {
        let refused = p2p_call(&other_agent_tool, call).await.unwrap_err();
        assert!(refused.contains("managed by another owner"), "{refused}");
    }
    let overlay = p2p_call(&tool_a, json!({"action":"pairings_get","peer_id":peer_b}))
        .await
        .unwrap();
    assert_eq!(
        overlay["outcome"]["desired"].as_array().map(Vec::len),
        Some(1)
    );
    let sibling = ConfigAccess::local_managed_agent(
        db_a.node.clone(),
        identity::Did::new(other_agent.did()).expect("sibling DID"),
        other_agent.as_ref(),
    )
    .expect("sibling access");
    let deleted = sibling
        .write(
            "test.pairing.sibling_delete",
            &format!(
                r#"mutation {{ delete_DataPlanePairingDesired(filter: {{peer_id: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
                escape_graphql_string(&peer_b)
            ),
        )
        .await
        .map_err(anyhow::Error::from);
    assert!(
        refused_or_empty(&deleted, "delete_DataPlanePairingDesired"),
        "{deleted:?}"
    );
    let kept = p2p_call(&tool_a, json!({"action":"pairings_get","peer_id":peer_b}))
        .await
        .unwrap();
    assert_eq!(kept["outcome"]["desired"], overlay["outcome"]["desired"]);
    let updated = sibling
        .write(
            "test.pairing.sibling_update",
            &format!(
                r#"mutation {{ update_DataPlanePairingDesired(filter: {{peer_id: {{_eq: "{}"}}}}, input: {{collections: ["ChangeNote", "GuardedChange"]}}) {{ _docID }} }}"#,
                escape_graphql_string(&peer_b)
            ),
        )
        .await
        .map_err(anyhow::Error::from);
    assert!(
        refused_or_empty(&updated, "update_DataPlanePairingDesired"),
        "{updated:?}"
    );
    let unchanged = p2p_call(&tool_a, json!({"action":"pairings_get","peer_id":peer_b}))
        .await
        .unwrap();
    assert_eq!(
        unchanged["outcome"]["desired"],
        overlay["outcome"]["desired"]
    );
    assert_eq!(overlay["outcome"]["desired"][0]["status"], "active");

    write_change_proposed(&db_b.node, "b-before-its-opt-in", "signup").await;
    assert_stays_local(
        &[&db_a.node],
        "ChangeProposed",
        || async { !has_change(&db_a.node, "b-before-its-opt-in").await },
        "a node that never opted in pushed its application documents",
    )
    .await;

    p2p_call(
        &tool_b,
        pairing_args(
            "pairings_apply",
            &peer_a,
            identity_a.did(),
            &["ChangeProposed"],
        ),
    )
    .await
    .expect("apply reverse pairing");
    wait_for_change(&db_a.node, "b-before-its-opt-in").await;
    write_change_proposed(&db_a.node, "both-a", "signup").await;
    write_change_proposed(&db_b.node, "both-b", "signup").await;
    wait_for_change(&db_b.node, "both-a").await;
    wait_for_change(&db_a.node, "both-b").await;

    let revoked = p2p_call(
        &tool_a,
        json!({"action":"pairings_revoke","peer_id":peer_b}),
    )
    .await
    .expect("revoke pairing");
    assert_eq!(revoked["outcome"]["unchanged"], false);
    wait_for_carry(&db_a.node, &peer_b, "ChangeProposed", false).await;
    write_change_proposed(&db_a.node, "after-revoke", "signup").await;
    assert_stays_local(
        &[&db_b.node],
        "ChangeProposed",
        || async { !has_change(&db_b.node, "after-revoke").await },
        "a revoked pairing replicated new data",
    )
    .await;
    assert!(
        GraphqlEnrollmentStore::new(db_a.node.clone(), identity_a.clone())
            .load_projection()
            .await
            .unwrap()
            .active
            .iter()
            .any(|entry| entry.request.request_id == enrollment_a_to_b.request_id),
        "revoking a pairing must not touch enrollment"
    );
    let idle = p2p_call(&tool_a, json!({"action":"pairings_get","peer_id":peer_b}))
        .await
        .unwrap();
    assert_eq!(idle["outcome"]["desired"], json!([]));
    write_change_proposed(&db_b.node, "b-still-flows", "signup").await;
    wait_for_change(&db_a.node, "b-still-flows").await;

    p2p_call(&tool_a, pairing("pairings_apply", &["ChangeProposed"]))
        .await
        .expect("apply again before the enrollment ends");
    wait_for_carry(&db_a.node, &peer_b, "ChangeProposed", true).await;
    GraphqlEnrollmentStore::new(db_a.node.clone(), identity_a.clone())
        .revoke_request(&enrollment_a_to_b.request_id)
        .await
        .expect("revoke exact approved enrollment generation");
    wait_for_revoked_route_teardown(
        &db_a.node,
        &db_b.node,
        &enrollment_a_to_b.request_id,
        &peer_b,
        Duration::from_secs(60),
    )
    .await;
    assert!(!replicator_carries(&db_a.node, &peer_b, "ChangeProposed").await);
    authorize_enrollment_generation(
        db_a.node.clone(),
        EnrollmentGeneration {
            network_id: NETWORK_ID,
            network_name: NETWORK_NAME,
            label: "-again",
            lease: Duration::from_secs(
                gents_protocol::enrollment::DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
            ),
        },
        identity_a.clone(),
        identity_b.clone(),
        &peer_b,
        &addr_b,
    )
    .await;
    wait_for_enrollment_route_applied(&db_a.node, &peer_b, Duration::from_secs(60)).await;
    wait_for_status(&db_a, &tool_a, &peer_b, Some("binding_ended")).await;
    write_change_proposed(&db_a.node, "after-reapproval", "signup").await;
    assert_never_carried(&db_a.node, &peer_b, "ChangeProposed").await;
    assert!(
        !has_change(&db_b.node, "after-reapproval").await,
        "a pairing revived after its enrollment was revoked and approved again"
    );

    let user_a = load_managed_fixture_user(&db_a).await;
    let removed = db_a
        .client_access(&user_a)
        .remove_application_pairing(&peer_b)
        .await
        .expect("the User removes the Agent's pairing");
    assert_eq!(removed.len(), 1, "{removed:?}");
    let gone = p2p_call(&tool_a, json!({"action":"pairings_get","peer_id":peer_b}))
        .await
        .unwrap();
    assert_eq!(gone["outcome"]["desired"], json!([]));

    let _ = stop_a.send(true);
    let _ = stop_b.send(true);
    task_a.await.unwrap().unwrap();
    task_b.await.unwrap().unwrap();
    db_a.node.shutdown().await.expect("node shutdown");
    db_b.node.shutdown().await.expect("node shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authorless_overlay_is_skipped_and_control_route_stands() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");

    let db_a = test_p2p_db("app-collection-soft-skip-a").await;
    let db_b = test_p2p_db("app-collection-soft-skip-b").await;

    let identity_a = db_a.node_identity.clone();
    let identity_b = db_b.node_identity.clone();
    let did_a = identity_a.did().to_string();

    let mock_a = MockModelEndpoint::start("default").unwrap();
    let mock_b = MockModelEndpoint::start("default").unwrap();
    bind_default_agent_backend(&db_a, "backend-soft-a", mock_a.endpoint()).await;
    bind_default_agent_backend(&db_b, "backend-soft-b", mock_b.endpoint()).await;

    let (stop_a, task_a) = start_runtime(&db_a).await;
    let (stop_b, task_b) = start_runtime(&db_b).await;

    let (peer_a, addr_a) = wait_for_peer_identity(&db_a.node).await;
    let (peer_b, addr_b) = wait_for_peer_identity(&db_b.node).await;

    authorize_enrollment_peer(
        db_a.node.clone(),
        "net-soft-skip",
        "Soft Skip Net",
        identity_a.clone(),
        identity_b.clone(),
        &peer_b,
        &addr_b,
    )
    .await;
    authorize_enrollment_peer(
        db_b.node.clone(),
        "net-soft-skip",
        "Soft Skip Net",
        identity_b.clone(),
        identity_a.clone(),
        &peer_a,
        &addr_a,
    )
    .await;

    wait_for_enrollment_route_applied(&db_a.node, &peer_b, Duration::from_secs(60)).await;
    let before = fetch_pairing_applied(&db_a.node, &peer_b)
        .await
        .expect("control applied");

    let peer = escape_graphql_string(&peer_b);
    let did = escape_graphql_string(&did_a);
    let addr = escape_graphql_string(&addr_b);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let resp = write(
        &db_a.node,
        &format!(
            r#"mutation {{
            create_DataPlanePairingDesired(input: {{
                peer_id: "{peer}", node_did: "{did}",
                collections: ["   "], replicator_addresses: ["{addr}"],
                template: "app-collections", source: "test-app-collections",
                created_at: "{now}", updated_at: "{now}"
            }}) {{ _docID }}
        }}"#
        ),
    )
    .await;
    assert!(
        !resp.has_errors(),
        "create blank DataPlanePairingDesired: {:?}",
        resp.errors
    );

    assert_stays_local(
        &[&db_a.node],
        "PeerPairingApplied",
        || async { fetch_pairing_applied(&db_a.node, &peer_b).await.as_ref() == Some(&before) },
        "an authorless overlay changed the control pairing",
    )
    .await;
    let after = fetch_pairing_applied(&db_a.node, &peer_b)
        .await
        .expect("control applied after soft-skip");
    assert_eq!(
        before, after,
        "control replicator addresses should be unchanged"
    );
    assert!(
        EmbeddedRemoteP2pAdmin::new(db_a.node.clone())
            .list_replicators()
            .await
            .expect("list replicators")
            .iter()
            .all(|replicator| replicator
                .collections
                .iter()
                .all(|collection| !collection.trim().is_empty())),
        "a blank collection name reached the transport"
    );

    let _ = stop_a.send(true);
    let _ = stop_b.send(true);
    task_a.await.unwrap().unwrap();
    task_b.await.unwrap().unwrap();
    db_a.node.shutdown().await.expect("node shutdown");
    db_b.node.shutdown().await.expect("node shutdown");
}

async fn install_shared_fixture(
    db: &crate::support::TestDb,
    archive: &gents_pack::pack::archive::PackArchive,
) {
    use gents_pack::pack::{
        install_prepared_document_pack, load_pack_config, prepare_document_pack_install,
        DriftPolicy, InstalledVia, PackInferenceBindings, PackInstallOptions,
    };
    let owner = db.node_identity.did().to_string();
    let user = crate::support::load_managed_fixture_user(db).await;
    crate::support::grant_household_owner(db, &user).await;
    let config = load_pack_config(
        archive.manifest(),
        &PackInstallOptions {
            node_did: owner.clone(),
        },
        &|path| Ok(archive.asset(path)?.to_vec()),
        &|_| None,
    )
    .unwrap();
    let access = db.client_access(&user);
    let prepared = prepare_document_pack_install(
        &access,
        &owner,
        archive.manifest(),
        &config,
        &PackInferenceBindings::new(),
        &[],
        InstalledVia::Operator,
        None,
        Some(archive),
        &|_| None,
        &Default::default(),
    )
    .await
    .unwrap();
    install_prepared_document_pack(
        &access,
        &owner,
        archive,
        &[],
        &prepared,
        None,
        false,
        InstalledVia::Operator,
        DriftPolicy::Refuse,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    let packs = gents_pack::pack::installed_packs(None, Some((&access, &owner)))
        .await
        .unwrap();
    let pack = packs
        .iter()
        .find(|pack| pack.coordinate == "fixture/shared_fixture")
        .expect("the node records the installed pack");
    assert_eq!(pack.version, "1.0.0");
    assert_eq!(pack.digest, archive.digest());
}

async fn household_access(db: &crate::support::TestDb) -> ConfigAccess {
    let user = crate::support::load_managed_fixture_user(db).await;
    ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(db.client_endpoint(), user.did())
            .with_client_signing_capability(user.client_signing_capability().unwrap())
            .unwrap(),
    )
}

async fn write_fixture_move(db: &crate::support::TestDb, id: &str, mark: &str) {
    let id = escape_graphql_string(id);
    let mark = escape_graphql_string(mark);
    let mutation = format!(
        r#"mutation {{ create_SharedFixtureMove(input: {{ move_id: "{id}", mark: "{mark}" }}) {{ _docID }} }}"#
    );
    household_access(db)
        .await
        .write("test.shared_fixture_move", &mutation)
        .await
        .unwrap();
}

async fn fixture_moves(db: &crate::support::TestDb, id: &str) -> Vec<Value> {
    let escaped = escape_graphql_string(id);
    let query = format!(
        r#"{{ SharedFixtureMove(filter: {{ move_id: {{ _eq: "{escaped}" }} }}) {{ move_id mark }} }}"#
    );
    household_access(db).await.execute(&query).await.unwrap()["data"]["SharedFixtureMove"]
        .as_array()
        .expect("move rows")
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_pack_collections_pair_only_by_explicit_opt_in() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/packs/shared_fixture");
    let (bytes, _) = gents_pack::pack::archive::pack_dir(&path).unwrap();
    let archive = gents_pack::pack::archive::PackArchive::from_bytes(&bytes).unwrap();
    let a = test_p2p_db("shared-fixture-a").await;
    let b = test_p2p_db("shared-fixture-b").await;
    install_shared_fixture(&a, &archive).await;
    install_shared_fixture(&b, &archive).await;

    let mock_a = MockModelEndpoint::start("default").unwrap();
    let mock_b = MockModelEndpoint::start("default").unwrap();
    bind_default_agent_backend(&a, "fixture-a-backend", mock_a.endpoint()).await;
    bind_default_agent_backend(&b, "fixture-b-backend", mock_b.endpoint()).await;
    let (stop_a, task_a) = start_runtime(&a).await;
    let (stop_b, task_b) = start_runtime(&b).await;

    let (peer_a, address_a) = wait_for_peer_identity(&a.node).await;
    let (peer_b, address_b) = wait_for_peer_identity(&b.node).await;
    let enrollment = authorize_enrollment_peer(
        a.node.clone(),
        NETWORK_ID,
        NETWORK_NAME,
        a.node_identity.clone(),
        b.node_identity.clone(),
        &peer_b,
        &address_b,
    )
    .await;
    authorize_enrollment_peer(
        b.node.clone(),
        NETWORK_ID,
        NETWORK_NAME,
        b.node_identity.clone(),
        a.node_identity.clone(),
        &peer_a,
        &address_a,
    )
    .await;
    wait_for_enrollment_route_applied(&a.node, &peer_b, Duration::from_secs(60)).await;
    wait_for_current_route_receipt(
        &b.node,
        &enrollment.request_id,
        &enrollment.request_digest,
        &peer_b,
        Duration::from_secs(60),
    )
    .await;
    wait_for_enrollment_route_applied(&b.node, &peer_a, Duration::from_secs(60)).await;
    assert_never_carried(&a.node, &peer_b, "SharedFixtureMove").await;
    assert_never_carried(&b.node, &peer_a, "SharedFixtureMove").await;
    refuse_at_transport(&a.node, &address_b, "AgentSession").await;
    refuse_at_transport(&b.node, &address_a, "AgentSession").await;

    write_fixture_move(&a, "move-a", "X").await;
    write_fixture_move(&b, "move-b", "O").await;
    assert_stays_local(
        &[&a.node, &b.node],
        "SharedFixtureMove",
        || async {
            fixture_moves(&b, "move-a").await.is_empty()
                && fixture_moves(&a, "move-b").await.is_empty()
        },
        "a pack document replicated to the paired peer",
    )
    .await;
    assert_eq!(fixture_moves(&a, "move-a").await.len(), 1);
    assert_eq!(fixture_moves(&b, "move-b").await.len(), 1);

    let pairer = pairing_agent(&a, "pack-pairer").await;
    let tool = pairing_tool(&a, Some(pairer.identity.clone()), true);
    p2p_call(
        &tool,
        pairing_args(
            "pairings_apply",
            &peer_b,
            b.node_identity.did(),
            &["SharedFixtureMove"],
        ),
    )
    .await
    .expect("pair the pack collection");
    wait_for_status(&a, &tool, &peer_b, None).await;
    wait_until(
        &b.node,
        &["SharedFixtureMove"],
        "the pack document to arrive",
        || async { !fixture_moves(&b, "move-a").await.is_empty() },
    )
    .await;
    assert!(fixture_moves(&a, "move-b").await.is_empty());

    let _ = stop_a.send(true);
    let _ = stop_b.send(true);
    task_a.await.unwrap().unwrap();
    task_b.await.unwrap().unwrap();
    a.node.shutdown().await.unwrap();
    b.node.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_pairing_replicates_only_while_its_author_holds_the_grant() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");

    let db_a = test_p2p_db("app-pairing-authority-a").await;
    let db_b = test_p2p_db("app-pairing-authority-b").await;
    for db in [&db_a, &db_b] {
        register_change_proposed_schema(&db.node).await;
        register_guarded_change_schema(&db.node).await;
    }
    let mock_a = MockModelEndpoint::start("default").unwrap();
    let mock_b = MockModelEndpoint::start("default").unwrap();
    bind_default_agent_backend(&db_a, "backend-authority-a", mock_a.endpoint()).await;
    bind_default_agent_backend(&db_b, "backend-authority-b", mock_b.endpoint()).await;
    let pairer_a = pairing_agent(&db_a, "authority-a").await;
    let pairer_b = pairing_agent(&db_b, "authority-b").await;
    let tool_a = pairing_tool(&db_a, Some(pairer_a.identity.clone()), true);
    let tool_b = pairing_tool(&db_b, Some(pairer_b.identity.clone()), true);

    let (stop_a, task_a) = start_runtime(&db_a).await;
    let (stop_b, task_b) = start_runtime(&db_b).await;

    let (peer_a, _) = wait_for_peer_identity(&db_a.node).await;
    let (peer_b, addr_b) = wait_for_peer_identity(&db_b.node).await;
    authorize_enrollment_peer(
        db_a.node.clone(),
        NETWORK_ID,
        NETWORK_NAME,
        db_a.node_identity.clone(),
        db_b.node_identity.clone(),
        &peer_b,
        &addr_b,
    )
    .await;
    wait_for_enrollment_route_applied(&db_a.node, &peer_b, Duration::from_secs(60)).await;
    let one_way = p2p_call(
        &tool_b,
        pairing_args(
            "pairings_apply",
            &peer_a,
            db_a.node_identity.did(),
            &["ChangeProposed"],
        ),
    )
    .await
    .unwrap_err();
    assert!(one_way.contains("enrollment"), "{one_way}");
    let listed = p2p_call(&tool_b, json!({"action":"pairings_list"}))
        .await
        .expect("pairings_list");
    assert_eq!(listed["outcome"]["desired"], json!([]));

    p2p_call(
        &tool_a,
        pairing_args(
            "pairings_apply",
            &peer_b,
            db_b.node_identity.did(),
            &["ChangeProposed"],
        ),
    )
    .await
    .expect("apply pairing");
    wait_for_status(&db_a, &tool_a, &peer_b, None).await;
    write_change_proposed(&db_a.node, "first", "signup").await;
    wait_for_change(&db_b.node, "first").await;
    wait_for_status(&db_a, &tool_a, &peer_b, None).await;

    authorize_enrollment_generation(
        db_a.node.clone(),
        EnrollmentGeneration {
            network_id: NETWORK_ID,
            network_name: NETWORK_NAME,
            label: "-renewal",
            lease: Duration::from_secs(
                gents_protocol::enrollment::DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
            ),
        },
        db_a.node_identity.clone(),
        db_b.node_identity.clone(),
        &peer_b,
        &addr_b,
    )
    .await;
    let (_, renewed, _) = current_authorization(&db_a, &peer_b).await;
    assert!(renewed > 1, "renewal took the next sequence");
    write_change_proposed(&db_a.node, "after-renewal", "signup").await;
    wait_for_change(&db_b.node, "after-renewal").await;
    wait_for_status(&db_a, &tool_a, &peer_b, None).await;

    pairer_a.configure(&db_a, false, true).await;
    wait_for_status(&db_a, &tool_a, &peer_b, Some("author_lacks_grant")).await;
    wait_for_carry(&db_a.node, &peer_b, "ChangeProposed", false).await;
    write_change_proposed(&db_a.node, "while-withdrawn", "signup").await;
    assert_stays_local(
        &[&db_b.node],
        "ChangeProposed",
        || async { !has_change(&db_b.node, "while-withdrawn").await },
        "a pairing whose author lost the grant replicated",
    )
    .await;
    pairer_a.configure(&db_a, true, true).await;
    wait_for_status(&db_a, &tool_a, &peer_b, None).await;
    write_change_proposed(&db_a.node, "after-regrant", "signup").await;
    wait_for_change(&db_b.node, "after-regrant").await;

    pairer_a.configure(&db_a, true, false).await;
    wait_for_status(&db_a, &tool_a, &peer_b, Some("author_not_agent")).await;
    wait_for_carry(&db_a.node, &peer_b, "ChangeProposed", false).await;
    pairer_a.configure(&db_a, true, true).await;
    wait_for_status(&db_a, &tool_a, &peer_b, None).await;
    wait_for_carry(&db_a.node, &peer_b, "ChangeProposed", true).await;

    let user = load_managed_fixture_user(&db_a).await;
    let operator = db_a.client_access(&user);
    let rebind = format!(
        r#"mutation {{ update_DataPlanePairingDesired(filter: {{peer_id: {{_eq: "{}"}}}}, input: {{collections: ["ChangeProposed", "GuardedChange"], enrollment_sequence: 99}}) {{ _docID }} }}"#,
        escape_graphql_string(&peer_b)
    );
    let before = pairing_status(&tool_a, &peer_b).await;
    let admin_key = db_a.data_path().join("pairing-admin.key");
    let admin = tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(admin_key, None))
        .await
        .expect("join admin key setup")
        .expect("admin key");
    runtime_access(&db_a)
        .grant_node_admin(&identity::Did::new(admin.did()).expect("admin DID"))
        .await
        .expect("grant node administration");
    for (who, access) in [("user", &operator), ("admin", &db_a.client_access(&admin))] {
        let rewritten = access
            .write("test.pairing.foreign_update", &rebind)
            .await
            .map_err(anyhow::Error::from);
        assert!(
            refused_or_empty(&rewritten, "update_DataPlanePairingDesired"),
            "{who}: {rewritten:?}"
        );
    }
    runtime_access(&db_a)
        .grant_node_permission(
            &identity::Did::new(user.did()).expect("user DID"),
            gents_store::config_client::NodePermission::NacStatus,
        )
        .await
        .expect("grant node grant explanation");
    let after = pairing_status(&tool_a, &peer_b).await;
    assert_eq!(after["collections"], before["collections"]);
    assert_eq!(after["enrollment_sequence"], before["enrollment_sequence"]);
    assert_eq!(after["status"], "active");
    assert!(replicator_carries(&db_a.node, &peer_b, "ChangeProposed").await);
    assert!(!replicator_carries(&db_a.node, &peer_b, "GuardedChange").await);
    let listed = gents_p2p::p2p_reconcile::overlay_binding::operator_pairing_statuses(&operator)
        .await
        .expect("operator listing");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["status"], "active", "{listed:?}");

    let observer = pairing_agent(&db_a, "observer-a").await;
    let observer_tool = pairing_tool(&db_a, Some(observer.identity.clone()), false);
    pairer_a.configure(&db_a, false, true).await;
    wait_for_status(&db_a, &tool_a, &peer_b, Some("author_lacks_grant")).await;
    let seen = p2p_call(&observer_tool, json!({"action":"pairings_list"}))
        .await
        .expect("observer pairings_list");
    for row in seen["outcome"]["desired"].as_array().unwrap() {
        assert!(row.get("inactive_reason").is_none(), "{row}");
        assert!(row.get("inactive_detail").is_none(), "{row}");
    }
    let listed = gents_p2p::p2p_reconcile::overlay_binding::operator_pairing_statuses(&operator)
        .await
        .expect("operator listing");
    assert_eq!(
        listed[0]["inactive_reason"], "author_lacks_grant",
        "{listed:?}"
    );
    pairer_a.configure(&db_a, true, true).await;
    wait_for_status(&db_a, &tool_a, &peer_b, None).await;

    assert_eq!(
        operator
            .remove_application_pairing(&peer_b)
            .await
            .expect("remove")
            .len(),
        1
    );
    wait_for_carry(&db_a.node, &peer_b, "ChangeProposed", false).await;
    for (collection, template, reason) in [
        ("AgentSession", "app-collections", "collections_refused"),
        ("GuardedChange", "app-collections", "collections_refused"),
        ("ChangeProposed", "client", "invalid_template"),
        ("ChangeProposed", "no-such-template", "invalid_template"),
    ] {
        write_direct_overlay(&db_a, &pairer_a, &peer_b, collection, template).await;
        wait_for_status(&db_a, &tool_a, &peer_b, Some(reason)).await;
        fetch_pairing_applied(&db_a.node, &peer_b)
            .await
            .expect("control route stays applied");
        assert_never_carried(&db_a.node, &peer_b, collection).await;
        if reason == "collections_refused" {
            refuse_at_transport(&db_a.node, &addr_b, collection).await;
        }
        operator
            .remove_application_pairing(&peer_b)
            .await
            .expect("remove direct overlay");
    }

    let _ = stop_a.send(true);
    let _ = stop_b.send(true);
    task_a.await.unwrap().unwrap();
    task_b.await.unwrap().unwrap();
    db_a.node.shutdown().await.expect("node shutdown");
    db_b.node.shutdown().await.expect("node shutdown");
}
