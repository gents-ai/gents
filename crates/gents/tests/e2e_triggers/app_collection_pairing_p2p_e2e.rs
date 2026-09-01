//! #657 — reconcile-driven app-collection replication fires an EventTrigger.
//!
//! Unlike `event_trigger_p2p_e2e` (manual `install_one_way_replicator`), this
//! drives replication through `DataPlanePairingDesired` + the pairing
//! reconciler. Both nodes run `Gents::run`.

use std::time::{Duration, Instant};

use gents::agent::p2p_reconcile::GraphqlEnrollmentStore;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::graphql::graphql_response_with_transaction_retry as execute_graphql_with_conflict_retry;
use gents::{DocumentRuntimeOptions, Gents, ToolCeiling};
use serde::Deserialize;
use serde_json::Value;

use crate::support::enrollment::{authorize_enrollment_peer, wait_for_peer_identity};
use crate::support::fixtures::bind_default_behavior_backend;
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::{fetch_runtime_snapshot, RuntimeSnapshot};
use crate::support::test_p2p_db;

const TRIGGER_ID: &str = "trigger-app-collection-pairing";
const TASK_ID: &str = "task-app-collection-pairing";
const EXTERNAL_ID: &str = "change-app-1";
const PROMPT_TEMPLATE: &str = "fired for {{ doc.external_id }}";
const NETWORK_ID: &str = "net-app-collection-pairing";
const NETWORK_NAME: &str = "App Collection Pairing Net";
const HYDRATION_NETWORK_ID: &str = "net-enrollment-session-hydration";
const HYDRATION_SESSION_ID: &str = "session-enrollment-hydration";

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

/// Write a DataPlanePairingDesired row via desired-state config (NOT add_replicator).
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
                template: "app-collections", source: "test-app-collections",
                created_at: "{now}", updated_at: "{now}"
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

async fn wait_for_subscribed_collections(
    node: &EmbeddedNode,
    expected: &[String],
    timeout: Duration,
) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let subscribed = fetch_subscribed_collection_names(node).await;
        if expected
            .iter()
            .all(|collection| subscribed.contains(collection))
        {
            return subscribed;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for subscribed collections {expected:?}; last={subscribed:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
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

async fn wait_for_enrollment_route_applied(
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
            if has_addr {
                return Vec::new();
            }
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for enrollment PeerPairingApplied({peer_id}); last={last}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_current_route_receipt(
    node: &EmbeddedNode,
    request_id: &str,
    request_digest: &str,
    member_peer: &str,
    timeout: Duration,
) {
    let request_id = escape_graphql_string(request_id);
    let deadline = Instant::now() + timeout;
    loop {
        let response = node
            .execute(&format!(
                r#"{{ NetworkEnrollmentRouteReceipt(
                    filter: {{ request_id: {{ _eq: "{request_id}" }} }}
                ) {{
                    request_digest member_peer authorization_sequence
                    authorization_expires_at direction signer_did admin_sig
                }} }}"#
            ))
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
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_revoked_route_teardown(
    server: &EmbeddedNode,
    member: &EmbeddedNode,
    request_id: &str,
    member_peer: &str,
    timeout: Duration,
) {
    let request_id = escape_graphql_string(request_id);
    let member_peer = escape_graphql_string(member_peer);
    let deadline = Instant::now() + timeout;
    loop {
        let server_response = server
            .execute(&format!(
                r#"{{
                    PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{member_peer}" }} }}) {{ peer_id }}
                    PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{member_peer}" }} }}) {{ peer_id }}
                }}"#
            ))
            .await;
        let member_response = member
            .execute(&format!(
                r#"{{ NetworkAuthorizationRevision(
                    filter: {{ request_id: {{ _eq: "{request_id}" }}, kind: {{ _eq: "revoked" }} }}
                ) {{ sequence kind signer_did admin_sig }} }}"#
            ))
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
async fn app_collection_pairing_fires_event_trigger_via_reconcile() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    // Compress pairing sweeps for this process (read once at daemon start).
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");

    let db_a = test_p2p_db("app-collection-pairing-a").await;
    let db_b = test_p2p_db("app-collection-pairing-b").await;
    register_change_proposed_schema(db_a.node.as_ref()).await;
    register_change_proposed_schema(db_b.node.as_ref()).await;

    let identity_a = db_a.node_identity.clone();
    let identity_b = db_b.node_identity.clone();
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

    // Approve each live transport identity through authenticated enrollment.
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

    // B: document reconcile for Task + EventTrigger (ordering invariant).
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

    // The enrollment owner materializes the base client routes.
    let control_a =
        wait_for_enrollment_route_applied(db_a.node.as_ref(), &peer_b, Duration::from_secs(60))
            .await;
    wait_for_current_route_receipt(
        db_b.node.as_ref(),
        &enrollment_a_to_b.request_id,
        &enrollment_a_to_b.request_digest,
        &peer_b,
        Duration::from_secs(60),
    )
    .await;
    let control_b =
        wait_for_enrollment_route_applied(db_b.node.as_ref(), &peer_a, Duration::from_secs(60))
            .await;

    // App-collections data-plane rows on both sides.
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

    // Control subscriptions still present after app-collections merge. The
    // per-peer applied row is an ownership ledger, not the transport's global
    // subscription inventory, so assert against the actual P2P state.
    wait_for_subscribed_collections(db_a.node.as_ref(), &control_a, Duration::from_secs(30)).await;
    wait_for_subscribed_collections(db_b.node.as_ref(), &control_b, Duration::from_secs(30)).await;

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

    // Idempotence: applied state stable across another sweep window.
    let post = fetch_pairing_applied(db_a.node.as_ref(), &peer_b)
        .await
        .expect("post applied");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let post2 = fetch_pairing_applied(db_a.node.as_ref(), &peer_b)
        .await
        .expect("post2 applied");
    assert_eq!(post, post2, "pairing applied should be stable (idempotent)");
    wait_for_subscribed_collections(db_a.node.as_ref(), &control_a, Duration::from_secs(30)).await;

    GraphqlEnrollmentStore::new(db_a.node.clone(), identity_a.clone())
        .revoke_request(&enrollment_a_to_b.request_id)
        .await
        .expect("revoke exact approved enrollment generation");
    wait_for_revoked_route_teardown(
        db_a.node.as_ref(),
        db_b.node.as_ref(),
        &enrollment_a_to_b.request_id,
        &peer_b,
        Duration::from_secs(60),
    )
    .await;

    let _ = shutdown_a_tx.send(true);
    let _ = shutdown_b_tx.send(true);
    handle_a.await.unwrap().unwrap();
    handle_b.await.unwrap().unwrap();
    db_a.node.shutdown().await;
    db_b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_app_collection_row_does_not_stall_control_pairing() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");

    let db_a = test_p2p_db("app-collection-soft-skip-a").await;
    let db_b = test_p2p_db("app-collection-soft-skip-b").await;

    let identity_a = db_a.node_identity.clone();
    let identity_b = db_b.node_identity.clone();
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

    // Enrollment reconciler installs the base client route.
    let control =
        wait_for_enrollment_route_applied(db_a.node.as_ref(), &peer_b, Duration::from_secs(60))
            .await;
    let before = fetch_pairing_applied(db_a.node.as_ref(), &peer_b)
        .await
        .expect("control applied");

    // Blank-only collections: schema allows [String!]!, resolver soft-skips.
    let peer = escape_graphql_string(&peer_b);
    let did = escape_graphql_string(&did_a);
    let addr = escape_graphql_string(&addr_b);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_DataPlanePairingDesired(input: {{
                peer_id: "{peer}", agent_did: "{did}",
                collections: ["   "], replicator_addresses: ["{addr}"],
                template: "app-collections", source: "test-app-collections",
                created_at: "{now}", updated_at: "{now}"
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

    // Wait at least one sweep for the soft-skip path to run.
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
    db_a.node.shutdown().await;
    db_b.node.shutdown().await;
}

#[derive(Debug, Deserialize)]
struct HydrationStatusRow {
    status: Option<String>,
    served_doc_count: Option<i64>,
    status_detail: Option<String>,
}

async fn seed_preexisting_hydration_history(
    node: &EmbeddedNode,
    requester_did: &str,
    agent_did: &str,
    behavior_id: &str,
) {
    let requester_did = escape_graphql_string(requester_did);
    let agent_did = escape_graphql_string(agent_did);
    let behavior_id = escape_graphql_string(behavior_id);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = format!(
        r#"mutation {{
            session: create_AgentSession(input: {{
                session_id: "{HYDRATION_SESSION_ID}",
                requester_did: "{requester_did}",
                agent_did: "{agent_did}",
                agent_name: "hydration-runtime",
                behavior_id: "{behavior_id}",
                started: "{now}",
                status: "active"
            }}) {{ _docID }}
            message: create_AgentMessage(input: {{
                message_key: "{HYDRATION_SESSION_ID}:1",
                requester_did: "{requester_did}",
                agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                session_id: "{HYDRATION_SESSION_ID}",
                sequence: 1,
                role: "assistant",
                content: "pre-existing authenticated hydration history",
                timestamp: "{now}"
            }}) {{ _docID }}
        }}"#,
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed pre-existing hydration history: {:?}",
        response.errors
    );
}

async fn create_session_hydration_request(
    node: &EmbeddedNode,
    peer_id: &str,
    requester_did: &str,
    agent_did: &str,
) -> String {
    let request_key = format!("{peer_id}:{HYDRATION_SESSION_ID}");
    let request_key_gql = escape_graphql_string(&request_key);
    let requester_did = escape_graphql_string(requester_did);
    let agent_did = escape_graphql_string(agent_did);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let response = node
        .execute(&format!(
            r#"mutation {{
                create_SessionHydrationRequest(input: {{
                    request_key: "{request_key_gql}",
                    requester_did: "{requester_did}",
                    agent_did: "{agent_did}",
                    session_id: "{HYDRATION_SESSION_ID}",
                    created_at: "{now}",
                    status: "pending",
                    status_detail: "",
                    served_doc_count: 0
                }}) {{ _docID }}
            }}"#,
        ))
        .await;
    assert!(
        !response.has_errors(),
        "create session hydration request: {:?}",
        response.errors
    );
    request_key
}

async fn hydration_status(node: &EmbeddedNode, request_key: &str) -> Option<HydrationStatusRow> {
    let request_key = escape_graphql_string(request_key);
    let response = node
        .execute(&format!(
            r#"{{ SessionHydrationRequest(
                filter: {{ request_key: {{ _eq: "{request_key}" }} }}, limit: 1
            ) {{ status served_doc_count status_detail }} }}"#,
        ))
        .await;
    assert!(
        !response.has_errors(),
        "query hydration status: {:?}",
        response.errors
    );
    crate::support::first_optional_row(&response, "SessionHydrationRequest")
}

async fn wait_for_hydrated_history(
    node: &EmbeddedNode,
    request_key: &str,
    requester_did: &str,
    agent_did: &str,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    let requester_did = escape_graphql_string(requester_did);
    let agent_did = escape_graphql_string(agent_did);
    loop {
        let status = hydration_status(node, request_key).await;
        let response = node
            .execute(&format!(
                r#"{{ AgentMessage(filter: {{
                    requester_did: {{ _eq: "{requester_did}" }},
                    agent_did: {{ _eq: "{agent_did}" }},
                    session_id: {{ _eq: "{HYDRATION_SESSION_ID}" }}
                }}) {{ content }} }}"#,
            ))
            .await;
        assert!(
            !response.has_errors(),
            "query hydrated history: {:?}",
            response.errors
        );
        let message_present = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    row.get("content").and_then(Value::as_str)
                        == Some("pre-existing authenticated hydration history")
                })
            });
        if status.as_ref().is_some_and(|row| {
            row.status.as_deref() == Some("served")
                && row.served_doc_count.is_some_and(|count| count >= 1)
        }) && message_present
        {
            return;
        }
        let last = format!("status={status:?}, message_present={message_present}");
        assert!(
            Instant::now() < deadline,
            "timed out waiting for authenticated session hydration; last={last}"
        );
        if let Some(detail) = status
            .as_ref()
            .filter(|row| row.status.as_deref() == Some("rejected"))
            .and_then(|row| row.status_detail.as_deref())
        {
            panic!("authenticated session hydration rejected: {detail}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_enrollment_desired(node: &EmbeddedNode, peer_id: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let peer_id = escape_graphql_string(peer_id);
    loop {
        let response = node
            .execute(&format!(
                r#"{{ PeerPairingDesired(filter: {{
                    peer_id: {{ _eq: "{peer_id}" }}, source: {{ _eq: "enrollment" }}
                }}) {{ peer_id source }} }}"#,
            ))
            .await;
        let ready = response
            .data
            .as_ref()
            .and_then(|data| data.get("PeerPairingDesired"))
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.len() == 1);
        if ready {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for enrollment-owned hydration route; errors={:?}",
            response.errors
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn install_control_only_authenticated_hydration_route(
    server: &EmbeddedNode,
    client: &EmbeddedNode,
    client_peer: &str,
    client_addr: &str,
    requester_did: &str,
    agent_did: &str,
) {
    use gents::agent::p2p_reconcile::{
        resolve_template, resolve_template_filters, PairingDirection, CLIENT_COLLECTIONS,
        CLIENT_TEMPLATE, CLIENT_TO_RUNTIME_COLLECTIONS,
    };

    let server_addr = server
        .p2p()
        .expect("server P2P")
        .shareable_address()
        .await
        .expect("server address lookup")
        .expect("server shareable address");
    let client_p2p = client.p2p().expect("client P2P");
    let server_p2p = server.p2p().expect("server P2P");
    client_p2p
        .connect_peer(&server_addr)
        .await
        .expect("connect hydration client to server");
    client_p2p
        .add_collections(
            CLIENT_COLLECTIONS
                .iter()
                .map(|collection| (*collection).to_string())
                .collect(),
        )
        .await
        .expect("subscribe hydration receiver collections");
    server_p2p
        .add_collections(vec!["SessionHydrationRequest".to_string()])
        .await
        .expect("subscribe hydration control collection");
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
        .expect("install client hydration control route");
    server_p2p
        .add_replicator(
            vec!["SessionHydrationRequest".to_string()],
            Some(client_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("install hydration status return route");

    let template = resolve_template(CLIENT_TEMPLATE).expect("client template");
    let filters = resolve_template_filters(
        template,
        PairingDirection::ClientToRuntime,
        requester_did,
        agent_did,
    );
    let filters = escape_graphql_string(&serde_json::to_string(&filters).expect("pairing filters"));
    let client_peer = escape_graphql_string(client_peer);
    let client_addr = escape_graphql_string(client_addr);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let response = server
        .execute(&format!(
            r#"mutation {{ create_PeerPairingApplied(input: {{
                peer_id: "{client_peer}",
                collections: ["SessionHydrationRequest"],
                replicator_addresses: ["{client_addr}"],
                replicator_filter: "{filters}",
                created_at: "{now}",
                updated_at: "{now}"
            }}) {{ _docID }} }}"#,
        ))
        .await;
    assert!(
        !response.has_errors(),
        "record control-only authenticated hydration route: {:?}",
        response.errors
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_enrollment_hydrates_preexisting_session_history() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");

    let server = test_p2p_db("enrollment-session-hydration-server").await;
    let client = test_p2p_db("enrollment-session-hydration-client").await;
    let server_identity = server.node_identity.clone();
    let client_identity = client.node_identity.clone();
    let server_did = server_identity.did().to_string();
    let client_did = client_identity.did().to_string();

    seed_preexisting_hydration_history(
        server.node.as_ref(),
        &client_did,
        &server_did,
        "behavior-enrollment-hydration",
    )
    .await;

    let (_server_peer, _server_addr) = wait_for_peer_identity(server.node.as_ref()).await;
    let (client_peer, client_addr) = wait_for_peer_identity(client.node.as_ref()).await;
    authorize_enrollment_peer(
        server.node.clone(),
        HYDRATION_NETWORK_ID,
        "Enrollment Session Hydration",
        server_identity.clone(),
        client_identity.clone(),
        &client_peer,
        &client_addr,
    )
    .await;

    let (authority_owner, authority) = gents::agent::p2p_reconcile::enrollment_authority_channel();
    let enrollment_cancel = tokio_util::sync::CancellationToken::new();
    let enrollment_handle = tokio::spawn(gents::agent::p2p_reconcile::run_enrollment_reconciler(
        server.node.clone(),
        server_identity.clone(),
        authority_owner,
        enrollment_cancel.clone(),
    ));
    wait_for_enrollment_desired(server.node.as_ref(), &client_peer, Duration::from_secs(30)).await;
    install_control_only_authenticated_hydration_route(
        server.node.as_ref(),
        client.node.as_ref(),
        &client_peer,
        &client_addr,
        &client_did,
        &server_did,
    )
    .await;

    let before = client
        .node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{HYDRATION_SESSION_ID}" }} }}) {{ _docID }} }}"#,
        ))
        .await;
    assert!(
        before
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "pre-existing history must not arrive before an explicit hydration request: {:?}",
        before.data
    );

    let hydration_cancel = tokio_util::sync::CancellationToken::new();
    let hydration_handle = tokio::spawn(
        gents::agent::p2p_reconcile::run_session_hydration_reconciler(
            server.node.clone(),
            authority,
            server_identity,
            hydration_cancel.clone(),
        ),
    );

    let request_key = create_session_hydration_request(
        client.node.as_ref(),
        &client_peer,
        &client_did,
        &server_did,
    )
    .await;
    wait_for_hydrated_history(
        client.node.as_ref(),
        &request_key,
        &client_did,
        &server_did,
        Duration::from_secs(60),
    )
    .await;

    hydration_cancel.cancel();
    enrollment_cancel.cancel();
    hydration_handle.await.unwrap().unwrap();
    enrollment_handle.await.unwrap().unwrap();
    server.node.shutdown().await;
    client.node.shutdown().await;
}
