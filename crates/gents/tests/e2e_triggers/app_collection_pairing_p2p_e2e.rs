use std::sync::Arc;
use std::time::{Duration, Instant};

use gents::agent::p2p_reconcile::templates::NETWORK_CONTROL_COLLECTIONS;
use gents::agent::p2p_reconcile::{GraphqlNetworkStore, NetworkStore};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::retry::execute_graphql_with_conflict_retry;
use gents::{AgentIdentity, DocumentRuntimeOptions, Gents, ToolCeiling};
use gents_protocol::network_token::{
    derive_membership_key, EndpointRecord, MembershipRecord, NetworkRecord,
};
use serde::Deserialize;
use serde_json::Value;

use crate::support::fixtures::{bind_default_behavior_backend, test_identity};
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::{fetch_runtime_snapshot, RuntimeSnapshot};
use crate::support::test_p2p_db;

const TRIGGER_ID: &str = "trigger-app-collection-pairing";
const TASK_ID: &str = "task-app-collection-pairing";
const EXTERNAL_ID: &str = "change-app-1";
const PROMPT_TEMPLATE: &str = "fired for {{ doc.external_id }}";
const NETWORK_ID: &str = "net-app-collection-pairing";
const NETWORK_NAME: &str = "App Collection Pairing Net";

fn bs58_sig(sig: &[u8]) -> String {
    bs58::encode(sig).into_string()
}

async fn register_change_proposed_schema(node: &EmbeddedNode) {
    // @branchable is REQUIRED — DefraDB only P2P-syncs branchable collections.
    let sdl = r#"
        type ChangeProposed @branchable {
            external_id: String
            payload: String
            kind: String @index
        }
    "#;
    node.add_schema(sdl)
        .await
        .expect("add_schema ChangeProposed");
}

async fn seed_materializable_peer(
    node: &EmbeddedNode,
    network_id: &str,
    admin_identity: &dyn AgentIdentity,
    member_identity: &dyn AgentIdentity,
    member_node_id: &str,
    member_address: &str,
) {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let mut network = NetworkRecord {
        network_id: network_id.to_string(),
        admin_did: admin_identity.did().to_string(),
        display_name: NETWORK_NAME.to_string(),
        default_template: "network-control".to_string(),
        created_at: now.clone(),
        sig: Vec::new(),
    };
    network.sig = admin_identity
        .sign(&network.signing_payload())
        .await
        .expect("sign AgentNetwork");

    let mut membership = MembershipRecord {
        network_id: network_id.to_string(),
        member_did: member_identity.did().to_string(),
        status: "active".to_string(),
        granted_at: now.clone(),
        revoked_at: String::new(),
        sig: Vec::new(),
    };
    membership.sig = admin_identity
        .sign(&membership.signing_payload())
        .await
        .expect("sign NetworkMembership");

    let mut endpoint = EndpointRecord {
        did: member_identity.did().to_string(),
        node_id: member_node_id.to_string(),
        address: member_address.to_string(),
        updated_at: now.clone(),
        sig: Vec::new(),
    };
    endpoint.sig = member_identity
        .sign(&endpoint.signing_payload())
        .await
        .expect("sign PeerEndpoint");

    let network_id_g = escape_graphql_string(&network.network_id);
    let admin_did = escape_graphql_string(&network.admin_did);
    let display_name = escape_graphql_string(&network.display_name);
    let default_template = escape_graphql_string(&network.default_template);
    let created_at = escape_graphql_string(&network.created_at);
    let admin_sig = escape_graphql_string(&bs58_sig(&network.sig));
    let network_mutation = format!(
        r#"mutation {{
            upsert_AgentNetwork(
                filter: {{ network_id: {{ _eq: "{network_id_g}" }} }},
                add: {{
                    network_id: "{network_id_g}",
                    admin_did: "{admin_did}",
                    display_name: "{display_name}",
                    default_template: "{default_template}",
                    created_at: "{created_at}",
                    admin_sig: "{admin_sig}"
                }},
                update: {{
                    admin_did: "{admin_did}",
                    display_name: "{display_name}",
                    default_template: "{default_template}",
                    created_at: "{created_at}",
                    admin_sig: "{admin_sig}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = execute_graphql_with_conflict_retry(
        node,
        &network_mutation,
        "seed materializable AgentNetwork",
    )
    .await;
    assert!(
        !resp.has_errors(),
        "upsert AgentNetwork failed: {:?}",
        resp.errors
    );

    let membership_key = escape_graphql_string(&derive_membership_key(
        &membership.network_id,
        &membership.member_did,
    ));
    let member_did = escape_graphql_string(&membership.member_did);
    let status = escape_graphql_string(&membership.status);
    let granted_at = escape_graphql_string(&membership.granted_at);
    let revoked_at = escape_graphql_string(&membership.revoked_at);
    let mem_sig = escape_graphql_string(&bs58_sig(&membership.sig));
    let mem_mutation = format!(
        r#"mutation {{
            upsert_NetworkMembership(
                filter: {{ membership_key: {{ _eq: "{membership_key}" }} }},
                add: {{
                    membership_key: "{membership_key}",
                    network_id: "{network_id_g}",
                    member_did: "{member_did}",
                    status: "{status}",
                    granted_at: "{granted_at}",
                    revoked_at: "{revoked_at}",
                    admin_sig: "{mem_sig}"
                }},
                update: {{
                    network_id: "{network_id_g}",
                    member_did: "{member_did}",
                    status: "{status}",
                    granted_at: "{granted_at}",
                    revoked_at: "{revoked_at}",
                    admin_sig: "{mem_sig}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = execute_graphql_with_conflict_retry(
        node,
        &mem_mutation,
        "seed materializable NetworkMembership",
    )
    .await;
    assert!(
        !resp.has_errors(),
        "upsert NetworkMembership failed: {:?}",
        resp.errors
    );

    let did = escape_graphql_string(&endpoint.did);
    let node_id = escape_graphql_string(&endpoint.node_id);
    let address = escape_graphql_string(&endpoint.address);
    let updated_at = escape_graphql_string(&endpoint.updated_at);
    let binding_sig = escape_graphql_string(&bs58_sig(&endpoint.sig));
    let ep_mutation = format!(
        r#"mutation {{
            upsert_PeerEndpoint(
                filter: {{ did: {{ _eq: "{did}" }} }},
                add: {{
                    did: "{did}",
                    node_id: "{node_id}",
                    address: "{address}",
                    updated_at: "{updated_at}",
                    binding_sig: "{binding_sig}"
                }},
                update: {{
                    node_id: "{node_id}",
                    address: "{address}",
                    updated_at: "{updated_at}",
                    binding_sig: "{binding_sig}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp =
        execute_graphql_with_conflict_retry(node, &ep_mutation, "seed materializable PeerEndpoint")
            .await;
    assert!(
        !resp.has_errors(),
        "upsert PeerEndpoint failed: {:?}",
        resp.errors
    );
}

async fn wait_for_peer_identity(node: &EmbeddedNode) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let p2p = node.p2p().expect("p2p enabled");
        let peer_id = p2p.local_peer_id().await.ok();
        let shareable = p2p.shareable_address().await.ok().flatten();
        if let (Some(peer_id), Some(address)) = (peer_id, shareable) {
            if !peer_id.trim().is_empty() && !address.trim().is_empty() {
                return (peer_id, address);
            }
        }
        if let Ok(addrs) = p2p.listen_addresses().await {
            if let Some(addr) = addrs.first() {
                if let Some(peer_id) = addr.rsplit("/p2p/").nth(1) {
                    if !peer_id.is_empty() {
                        return (peer_id.to_string(), addr.clone());
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            panic!("node never exposed a P2P peer identity");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn create_task(node: &EmbeddedNode, task_id: &str, behavior_id: &str, prompt_template: &str) {
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let escaped_prompt_template = escape_graphql_string(prompt_template);
    let mutation = format!(
        r#"mutation {{
            create_Task(input: {{
                task_id: "{escaped_task_id}",
                name: "{escaped_task_id}",
                behavior_id: "{escaped_behavior_id}",
                prompt_template: "{escaped_prompt_template}",
                enabled: true
            }}) {{ _docID }}
        }}"#
    );
    let response = execute_graphql_with_conflict_retry(node, &mutation, "create e2e Task").await;
    assert!(
        !response.has_errors(),
        "create Task failed: {:?}",
        response.errors
    );
}

async fn create_event_trigger_with_filter(
    node: &EmbeddedNode,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
    event_kind: &str,
    filter: &str,
) {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let escaped_task_id = escape_graphql_string(task_id);
    let escaped_source_collection = escape_graphql_string(source_collection);
    let escaped_event_kind = escape_graphql_string(event_kind);
    let escaped_filter = escape_graphql_string(filter);
    let mutation = format!(
        r#"mutation {{
            create_EventTrigger(input: {{
                trigger_id: "{escaped_trigger_id}",
                task_id: "{escaped_task_id}",
                source_collection: "{escaped_source_collection}",
                event_kind: "{escaped_event_kind}",
                filter: "{escaped_filter}",
                enabled: true,
                concurrency: "serial",
                fire_count: 0
            }}) {{ _docID }}
        }}"#
    );
    let response =
        execute_graphql_with_conflict_retry(node, &mutation, "create e2e EventTrigger").await;
    assert!(
        !response.has_errors(),
        "create EventTrigger failed: {:?}",
        response.errors
    );
}

async fn wait_for_runtime_snapshot<F>(
    node: &EmbeddedNode,
    agent_did: &str,
    predicate: F,
) -> RuntimeSnapshot
where
    F: Fn(&RuntimeSnapshot) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(snapshot) = fetch_runtime_snapshot(node, agent_did).await {
            if predicate(&snapshot) {
                return snapshot;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for runtime snapshot for {agent_did}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn write_app_collection_pairing(
    node: &EmbeddedNode,
    peer_id: &str,
    self_did: &str,
    address: &str,
    collections: &[&str],
) {
    assert!(
        collections.iter().any(|c| !c.trim().is_empty()),
        "write_app_collection_pairing is happy-path only; pass a non-empty collection set"
    );
    let peer = escape_graphql_string(peer_id);
    let did = escape_graphql_string(self_did);
    let addr = escape_graphql_string(address);
    let cols = collections
        .iter()
        .map(|c| format!(r#""{}""#, escape_graphql_string(c)))
        .collect::<Vec<_>>()
        .join(",");
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_DataPlanePairingDesired(input: {{
                peer_id: "{peer}", agent_did: "{did}",
                collections: [{cols}], replicator_addresses: ["{addr}"],
                template: "app-collections", created_at: "{now}", updated_at: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let resp =
        execute_graphql_with_conflict_retry(node, &mutation, "create e2e DataPlanePairingDesired")
            .await;
    assert!(
        !resp.has_errors(),
        "create DataPlanePairingDesired: {:?}",
        resp.errors
    );
}

async fn fetch_pairing_applied(
    node: &EmbeddedNode,
    peer_id: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    let escaped = escape_graphql_string(peer_id);
    let query = format!(
        r#"{{
            PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                peer_id
                collections
                replicator_addresses
            }}
        }}"#
    );
    let response = node.execute(&query).await;
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
    let collections = row
        .get("collections")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let addresses = row
        .get("replicator_addresses")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some((collections, addresses))
}

async fn fetch_subscribed_collection_names(node: &EmbeddedNode) -> Vec<String> {
    let Some(p2p) = node.p2p() else {
        return Vec::new();
    };
    let Ok(ids) = p2p.get_collections().await else {
        return Vec::new();
    };
    let Ok(names) = node.list_collections() else {
        return Vec::new();
    };
    names
        .into_iter()
        .filter(|name| {
            node.get_collection(name)
                .ok()
                .flatten()
                .is_some_and(|definition| ids.contains(&definition.collection_id))
        })
        .collect()
}

async fn wait_for_app_collections_pairing_applied(
    node: &EmbeddedNode,
    peer_id: &str,
    expected_collection: &str,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    let mut last = String::from("<none>");
    loop {
        if let Some((collections, addresses)) = fetch_pairing_applied(node, peer_id).await {
            let subscribed = fetch_subscribed_collection_names(node).await;
            last = format!(
                "applied_collections={collections:?} addresses={addresses:?} \
                 subscribed={subscribed:?}"
            );
            let has_addr = addresses.iter().any(|a| !a.trim().is_empty());
            let has_col = subscribed.iter().any(|c| c == expected_collection);
            if has_addr && has_col {
                return;
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for PeerPairingApplied({peer_id}) to install \
                 {expected_collection}; last={last}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_control_pairing_applied(
    node: &EmbeddedNode,
    peer_id: &str,
    timeout: Duration,
) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    let mut last = String::from("<none>");
    loop {
        if let Some((collections, addresses)) = fetch_pairing_applied(node, peer_id).await {
            let subscribed = fetch_subscribed_collection_names(node).await;
            last = format!(
                "applied_collections={collections:?} addresses={addresses:?} \
                 subscribed={subscribed:?}"
            );
            let has_addr = addresses.iter().any(|a| !a.trim().is_empty());
            let has_control = NETWORK_CONTROL_COLLECTIONS
                .iter()
                .all(|expected| subscribed.iter().any(|actual| actual == expected));
            if has_addr && has_control {
                return NETWORK_CONTROL_COLLECTIONS
                    .iter()
                    .map(|collection| (*collection).to_string())
                    .collect();
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for network-control PeerPairingApplied({peer_id}); last={last}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[derive(Debug, Deserialize)]
struct AgentRequestRow {
    #[serde(rename = "_docID")]
    _doc_id: String,
    request_id: String,
    content: String,
    caused_by_trigger_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
    execution_origin: Option<String>,
}

async fn query_agent_requests_for_trigger(
    node: &EmbeddedNode,
    trigger_id: &str,
) -> Vec<AgentRequestRow> {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_trigger_id: {{ _eq: "{escaped_trigger_id}" }} }}
            ) {{
                _docID
                request_id
                content
                caused_by_trigger_id
                caused_by_trigger_kind
                execution_origin
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "AgentRequest query failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rows.into_iter()
        .map(|row| serde_json::from_value(row).expect("decode AgentRequest row"))
        .collect()
}

#[derive(Debug, Deserialize)]
struct EventTriggerRow {
    fire_count: Option<i64>,
    last_status: Option<String>,
    last_fired_source_doc_id: Option<String>,
    last_error: Option<String>,
    task_id: Option<String>,
    source_collection: Option<String>,
    event_kind: Option<String>,
    enabled: Option<bool>,
    concurrency: Option<String>,
}

async fn fetch_event_trigger(node: &EmbeddedNode, trigger_id: &str) -> EventTriggerRow {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let query = format!(
        r#"{{
            EventTrigger(
                filter: {{ trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                limit: 1
            ) {{
                fire_count
                last_status
                last_fired_source_doc_id
                last_error
                task_id
                source_collection
                event_kind
                enabled
                concurrency
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "EventTrigger query failed: {:?}",
        response.errors
    );
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("EventTrigger"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .expect("EventTrigger row missing");
    serde_json::from_value(row).expect("decode EventTrigger row")
}

async fn write_change_proposed(node: &EmbeddedNode, external_id: &str, kind: &str) -> String {
    let escaped_external_id = escape_graphql_string(external_id);
    let escaped_kind = escape_graphql_string(kind);
    let mutation = format!(
        r#"mutation {{
            add_ChangeProposed(input: {{
                external_id: "{escaped_external_id}",
                payload: "{{}}",
                kind: "{escaped_kind}"
            }}) {{ _docID }}
        }}"#
    );
    let response =
        execute_graphql_with_conflict_retry(node, &mutation, "create e2e ChangeProposed").await;
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

async fn query_change_proposed(node: &EmbeddedNode) -> Vec<Value> {
    let response = node
        .execute(r#"{ ChangeProposed { external_id kind } }"#)
        .await;
    response
        .data
        .as_ref()
        .and_then(|data| data.get("ChangeProposed"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seed_makes_peer_materializable() {
    let db = test_p2p_db("app-collection-seed").await;
    let admin = test_identity("app-collection-seed-admin");
    let member = test_identity("app-collection-seed-member");
    seed_materializable_peer(
        db.node.as_ref(),
        "net-test",
        &admin,
        &member,
        "peer-node",
        "/ip4/127.0.0.1/tcp/9/p2p/peer-node",
    )
    .await;
    let store = GraphqlNetworkStore::new(db.node.clone(), Arc::new(admin));
    let entries = store
        .load_materializable_entries()
        .await
        .expect("load materializable");
    assert!(
        entries.iter().any(|e| e.peer_id == "peer-node"),
        "seeded peer must be materializable: {entries:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn app_collection_pairing_fires_event_trigger_via_reconcile() {
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");

    let db_a = test_p2p_db("app-collection-pairing-a").await;
    let db_b = test_p2p_db("app-collection-pairing-b").await;
    register_change_proposed_schema(db_a.node.as_ref()).await;
    register_change_proposed_schema(db_b.node.as_ref()).await;

    let identity_a: Arc<dyn AgentIdentity> = Arc::new(test_identity("app-collection-agent-a"));
    let identity_b: Arc<dyn AgentIdentity> = Arc::new(test_identity("app-collection-agent-b"));
    let admin: Arc<dyn AgentIdentity> = Arc::new(test_identity("app-collection-admin"));
    let did_a = identity_a.did().to_string();
    let did_b = identity_b.did().to_string();

    let mock_a = MockModelEndpoint::start("default").unwrap();
    let mock_b = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db_a.node.as_ref(),
        &did_a,
        "backend-app-collection-a",
        mock_a.endpoint(),
    )
    .await;
    bind_default_behavior_backend(
        db_b.node.as_ref(),
        &did_b,
        "backend-app-collection-b",
        mock_b.endpoint(),
    )
    .await;

    let agent_a = Gents::from_default_behavior_documents(
        db_a.node.clone(),
        identity_a.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_b = Gents::from_default_behavior_documents(
        db_b.node.clone(),
        identity_b.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let default_behavior_b = agent_b.default_behavior_id().to_string();
    let (shutdown_a_tx, shutdown_a_rx) = tokio::sync::watch::channel(false);
    let (shutdown_b_tx, shutdown_b_rx) = tokio::sync::watch::channel(false);
    let handle_a = tokio::spawn(agent_a.run(shutdown_a_rx));
    let handle_b = tokio::spawn(agent_b.run(shutdown_b_rx));

    let (peer_a, addr_a) = wait_for_peer_identity(db_a.node.as_ref()).await;
    let (peer_b, addr_b) = wait_for_peer_identity(db_b.node.as_ref()).await;

    seed_materializable_peer(
        db_a.node.as_ref(),
        NETWORK_ID,
        admin.as_ref(),
        identity_b.as_ref(),
        &peer_b,
        &addr_b,
    )
    .await;
    seed_materializable_peer(
        db_b.node.as_ref(),
        NETWORK_ID,
        admin.as_ref(),
        identity_a.as_ref(),
        &peer_a,
        &addr_a,
    )
    .await;

    let store_a = GraphqlNetworkStore::new(db_a.node.clone(), admin.clone());
    let store_b = GraphqlNetworkStore::new(db_b.node.clone(), admin.clone());
    let entries_a = store_a.load_materializable_entries().await.unwrap();
    let entries_b = store_b.load_materializable_entries().await.unwrap();
    assert!(
        entries_a.iter().any(|e| e.peer_id == peer_b),
        "A must materialize B: {entries_a:?}"
    );
    assert!(
        entries_b.iter().any(|e| e.peer_id == peer_a),
        "B must materialize A: {entries_b:?}"
    );

    let startup = wait_for_runtime_snapshot(db_b.node.as_ref(), &did_b, |s| {
        s.process_state == "ready" && s.reconcile_phase == "idle" && s.active_generation >= 1
    })
    .await;
    let initial_generation = startup.active_generation;

    create_task(
        db_b.node.as_ref(),
        TASK_ID,
        &default_behavior_b,
        PROMPT_TEMPLATE,
    )
    .await;
    create_event_trigger_with_filter(
        db_b.node.as_ref(),
        TRIGGER_ID,
        TASK_ID,
        "ChangeProposed",
        "created",
        r#"{ kind: { _eq: "signup" } }"#,
    )
    .await;
    wait_for_runtime_snapshot(db_b.node.as_ref(), &did_b, |s| {
        s.process_state == "ready"
            && s.reconcile_phase == "idle"
            && s.active_generation > initial_generation
            && s.last_reconcile_result == "applied"
    })
    .await;

    let control_a =
        wait_for_control_pairing_applied(db_a.node.as_ref(), &peer_b, Duration::from_secs(60))
            .await;
    let control_b =
        wait_for_control_pairing_applied(db_b.node.as_ref(), &peer_a, Duration::from_secs(60))
            .await;

    write_app_collection_pairing(
        db_a.node.as_ref(),
        &peer_b,
        &did_a,
        &addr_b,
        &["ChangeProposed"],
    )
    .await;
    write_app_collection_pairing(
        db_b.node.as_ref(),
        &peer_a,
        &did_b,
        &addr_a,
        &["ChangeProposed"],
    )
    .await;
    wait_for_app_collections_pairing_applied(
        db_a.node.as_ref(),
        &peer_b,
        "ChangeProposed",
        Duration::from_secs(45),
    )
    .await;
    wait_for_app_collections_pairing_applied(
        db_b.node.as_ref(),
        &peer_a,
        "ChangeProposed",
        Duration::from_secs(45),
    )
    .await;

    let subscribed_a = fetch_subscribed_collection_names(db_a.node.as_ref()).await;
    for col in &control_a {
        assert!(
            subscribed_a.contains(col),
            "control collection {col} missing after app-collections merge: {subscribed_a:?}"
        );
    }
    let subscribed_b = fetch_subscribed_collection_names(db_b.node.as_ref()).await;
    for col in &control_b {
        assert!(
            subscribed_b.contains(col),
            "control collection {col} missing after app-collections merge: {subscribed_b:?}"
        );
    }

    let source_doc_id = write_change_proposed(db_a.node.as_ref(), EXTERNAL_ID, "signup").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let requests = loop {
        let rows = query_agent_requests_for_trigger(db_b.node.as_ref(), TRIGGER_ID).await;
        if !rows.is_empty() {
            break rows;
        }
        if tokio::time::Instant::now() >= deadline {
            let on_b = query_change_proposed(db_b.node.as_ref()).await;
            let on_a = query_change_proposed(db_a.node.as_ref()).await;
            panic!(
                "timed out: P2P-replicated ChangeProposed did not fire B's trigger \
                 (source_doc_id={source_doc_id}).\n\
                 DIAGNOSTIC: ChangeProposed on A={on_a:?}\n\
                 DIAGNOSTIC: ChangeProposed on B={on_b:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        requests.len(),
        1,
        "expected exactly one AgentRequest, got {}: {:?}",
        requests.len(),
        requests
    );
    let request = &requests[0];
    assert_eq!(request.caused_by_trigger_id.as_deref(), Some(TRIGGER_ID));
    assert_eq!(request.caused_by_trigger_kind.as_deref(), Some("event"));
    assert_eq!(request.execution_origin.as_deref(), Some("scheduled"));
    assert_eq!(request.content, format!("fired for {EXTERNAL_ID}"));
    assert!(!request.request_id.is_empty());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let fired = loop {
        let row = fetch_event_trigger(db_b.node.as_ref(), TRIGGER_ID).await;
        if row.last_status.as_deref() == Some("fired") {
            break row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for EventTrigger.last_status=\"fired\" (last row: {row:?})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(fired.fire_count, Some(1));
    assert_eq!(
        fired.last_fired_source_doc_id.as_deref(),
        Some(source_doc_id.as_str())
    );
    assert!(fired.last_error.as_deref().unwrap_or("").is_empty());
    assert_eq!(fired.task_id.as_deref(), Some(TASK_ID));
    assert_eq!(fired.source_collection.as_deref(), Some("ChangeProposed"));
    assert_eq!(fired.event_kind.as_deref(), Some("created"));
    assert_eq!(fired.enabled, Some(true));
    assert_eq!(fired.concurrency.as_deref(), Some("serial"));

    let post = fetch_pairing_applied(db_a.node.as_ref(), &peer_b)
        .await
        .expect("post applied");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let post2 = fetch_pairing_applied(db_a.node.as_ref(), &peer_b)
        .await
        .expect("post2 applied");
    assert_eq!(post, post2, "pairing applied should be stable (idempotent)");
    let post_subscribed = fetch_subscribed_collection_names(db_a.node.as_ref()).await;
    for col in &control_a {
        assert!(
            post_subscribed.contains(col),
            "control subscription still present: {post_subscribed:?}"
        );
    }

    let _ = shutdown_a_tx.send(true);
    let _ = shutdown_b_tx.send(true);
    handle_a.await.unwrap().unwrap();
    handle_b.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_app_collection_row_does_not_stall_control_pairing() {
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");

    let db_a = test_p2p_db("app-collection-soft-skip-a").await;
    let db_b = test_p2p_db("app-collection-soft-skip-b").await;

    let identity_a: Arc<dyn AgentIdentity> = Arc::new(test_identity("app-collection-soft-a"));
    let identity_b: Arc<dyn AgentIdentity> = Arc::new(test_identity("app-collection-soft-b"));
    let admin: Arc<dyn AgentIdentity> = Arc::new(test_identity("app-collection-soft-admin"));
    let did_a = identity_a.did().to_string();
    let did_b = identity_b.did().to_string();

    let mock_a = MockModelEndpoint::start("default").unwrap();
    let mock_b = MockModelEndpoint::start("default").unwrap();
    bind_default_behavior_backend(
        db_a.node.as_ref(),
        &did_a,
        "backend-soft-a",
        mock_a.endpoint(),
    )
    .await;
    bind_default_behavior_backend(
        db_b.node.as_ref(),
        &did_b,
        "backend-soft-b",
        mock_b.endpoint(),
    )
    .await;

    let agent_a = Gents::from_default_behavior_documents(
        db_a.node.clone(),
        identity_a.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_b = Gents::from_default_behavior_documents(
        db_b.node.clone(),
        identity_b.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (shutdown_a_tx, shutdown_a_rx) = tokio::sync::watch::channel(false);
    let (shutdown_b_tx, shutdown_b_rx) = tokio::sync::watch::channel(false);
    let handle_a = tokio::spawn(agent_a.run(shutdown_a_rx));
    let handle_b = tokio::spawn(agent_b.run(shutdown_b_rx));

    let (peer_a, addr_a) = wait_for_peer_identity(db_a.node.as_ref()).await;
    let (peer_b, addr_b) = wait_for_peer_identity(db_b.node.as_ref()).await;

    seed_materializable_peer(
        db_a.node.as_ref(),
        "net-soft-skip",
        admin.as_ref(),
        identity_b.as_ref(),
        &peer_b,
        &addr_b,
    )
    .await;
    seed_materializable_peer(
        db_b.node.as_ref(),
        "net-soft-skip",
        admin.as_ref(),
        identity_a.as_ref(),
        &peer_a,
        &addr_a,
    )
    .await;

    let control =
        wait_for_control_pairing_applied(db_a.node.as_ref(), &peer_b, Duration::from_secs(60))
            .await;
    let before = fetch_pairing_applied(db_a.node.as_ref(), &peer_b)
        .await
        .expect("control applied");

    let peer = escape_graphql_string(&peer_b);
    let did = escape_graphql_string(&did_a);
    let addr = escape_graphql_string(&addr_b);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_DataPlanePairingDesired(input: {{
                peer_id: "{peer}", agent_did: "{did}",
                collections: ["   "], replicator_addresses: ["{addr}"],
                template: "app-collections", created_at: "{now}", updated_at: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = execute_graphql_with_conflict_retry(
        db_a.node.as_ref(),
        &mutation,
        "create blank e2e DataPlanePairingDesired",
    )
    .await;
    assert!(
        !resp.has_errors(),
        "create blank DataPlanePairingDesired: {:?}",
        resp.errors
    );

    tokio::time::sleep(Duration::from_secs(3)).await;

    let after = fetch_pairing_applied(db_a.node.as_ref(), &peer_b)
        .await
        .expect("control applied after soft-skip");
    for col in &control {
        assert!(
            after.0.contains(col),
            "control collection {col} lost after blank app-collections row: before={before:?} after={after:?}"
        );
    }
    assert!(
        !after.0.iter().any(|c| c == "ChangeProposed"),
        "blank app-collections must not install ChangeProposed: {after:?}"
    );
    assert_eq!(
        before.1, after.1,
        "control replicator addresses should be unchanged"
    );

    let _ = shutdown_a_tx.send(true);
    let _ = shutdown_b_tx.send(true);
    handle_a.await.unwrap().unwrap();
    handle_b.await.unwrap().unwrap();
}
