use std::time::{Duration, Instant};

use gents::defra_node::Node as EmbeddedNode;
use gents::runtime::agent::p2p_reconcile::{
    EmbeddedRemoteP2pAdmin, GraphqlEnrollmentStore, PairingFilters, RemoteP2pAdmin,
    RemoteP2pAdminError,
};
use gents::store::graphql::escape_graphql_string;
use gents::{DocumentRuntimeOptions, Gents, NodeIdentity, ToolCeiling};
use serde_json::Value;

use crate::support::enrollment::{authorize_enrollment_peer, wait_for_peer_identity};
use crate::support::fixtures::bind_default_agent_backend;
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::test_p2p_db;

const EXTERNAL_ID: &str = "change-app-1";
const NETWORK_ID: &str = "net-app-collection-pairing";
const NETWORK_NAME: &str = "App Collection Pairing Net";

async fn register_change_proposed_schema(node: &EmbeddedNode) {
    // @branchable is REQUIRED — DefraDB only P2P-syncs branchable collections.
    let sdl = r#"
        type ChangeProposed @branchable {
            external_id: String
            payload: String
            kind: String @index
        }
    "#;
    node.add_collection(sdl)
        .await
        .expect("add_collection ChangeProposed");
}

/// The P2P owner refuses to replicate an application or pack collection: only
/// discovery and enrollment documents cross runtimes.
async fn refuse_app_collection_replication(
    node: &std::sync::Arc<EmbeddedNode>,
    peer_address: &str,
    collection: &str,
) {
    let admin = EmbeddedRemoteP2pAdmin::new(node.clone());
    let refused = |result: Result<(), RemoteP2pAdminError>| matches!(result, Err(RemoteP2pAdminError::RuntimeLocalCollection(ref name)) if name == collection);
    assert!(
        refused(
            admin
                .add_replicator(
                    &[peer_address.to_owned()],
                    &[collection.to_owned()],
                    &PairingFilters::default(),
                )
                .await
        ),
        "{collection} must not be replicated"
    );
}

async fn fetch_pairing_applied(node: &EmbeddedNode, peer_id: &str) -> Option<Vec<String>> {
    let escaped = escape_graphql_string(peer_id);
    let query = format!(
        r#"{{
            PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                peer_id
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

async fn fetch_subscribed_collection_names(node: &EmbeddedNode) -> Vec<String> {
    // Subscription listing is authorized for the node's own identity only.
    let Some(p2p) = gents::store::identity::node_p2p(node) else {
        return Vec::new();
    };
    p2p.get_collections()
        .await
        .expect("list subscribed collections")
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

/// Several pairing sweeps pass without the application collection being
/// subscribed, and the control pairing stays applied.
async fn assert_app_collection_refused(
    node: &EmbeddedNode,
    peer_id: &str,
    collection: &str,
    control: &[String],
) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        fetch_pairing_applied(node, peer_id)
            .await
            .expect("control pairing stays applied");
        let subscribed = fetch_subscribed_collection_names(node).await;
        assert!(
            !subscribed.iter().any(|name| name == collection),
            "{collection} was subscribed: {subscribed:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    wait_for_subscribed_collections(node, control, Duration::from_secs(30)).await;
}

/// Holds `local` across several sync rounds; replication would break it.
async fn assert_stays_local<F, Fut>(local: F, violation: &str)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        assert!(local().await, "{violation}");
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
        if let Some(addresses) = fetch_pairing_applied(node, peer_id).await {
            let subscribed = fetch_subscribed_collection_names(node).await;
            last = format!("addresses={addresses:?} subscribed={subscribed:?}");
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
    let response = node.execute(&mutation).await;
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
async fn app_collection_pairing_never_replicates_application_documents() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");

    let db_a = test_p2p_db("app-collection-pairing-a").await;
    let db_b = test_p2p_db("app-collection-pairing-b").await;
    register_change_proposed_schema(db_a.node.as_ref()).await;
    register_change_proposed_schema(db_b.node.as_ref()).await;

    let identity_a = db_a.node_identity.clone();
    let identity_b = db_b.node_identity.clone();

    let mock_a = MockModelEndpoint::start("default").unwrap();
    let mock_b = MockModelEndpoint::start("default").unwrap();
    bind_default_agent_backend(&db_a, "backend-app-collection-a", mock_a.endpoint()).await;
    bind_default_agent_backend(&db_b, "backend-app-collection-b", mock_b.endpoint()).await;

    let agent_a = Gents::from_default_agent_documents(
        db_a.node.clone(),
        identity_a.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db_a.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_b = Gents::from_default_agent_documents(
        db_b.node.clone(),
        identity_b.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db_b.home.backend_admission()),
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

    refuse_app_collection_replication(&db_a.node, &addr_b, "ChangeProposed").await;
    refuse_app_collection_replication(&db_b.node, &addr_a, "ChangeProposed").await;
    // P2P carries only discovery and enrollment: the application collection
    // never enters the applied pairing and the control pairing stays as it was.
    assert_app_collection_refused(db_a.node.as_ref(), &peer_b, "ChangeProposed", &control_a).await;
    assert_app_collection_refused(db_b.node.as_ref(), &peer_a, "ChangeProposed", &control_b).await;

    write_change_proposed(db_a.node.as_ref(), EXTERNAL_ID, "signup").await;
    assert_stays_local(
        || async { query_change_proposed(db_b.node.as_ref()).await.is_empty() },
        "ChangeProposed replicated to the paired peer",
    )
    .await;
    assert_eq!(query_change_proposed(db_a.node.as_ref()).await.len(), 1);

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
    db_a.node.shutdown().await.expect("node shutdown");
    db_b.node.shutdown().await.expect("node shutdown");
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

    let mock_a = MockModelEndpoint::start("default").unwrap();
    let mock_b = MockModelEndpoint::start("default").unwrap();
    bind_default_agent_backend(&db_a, "backend-soft-a", mock_a.endpoint()).await;
    bind_default_agent_backend(&db_b, "backend-soft-b", mock_b.endpoint()).await;

    let agent_a = Gents::from_default_agent_documents(
        db_a.node.clone(),
        identity_a.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db_a.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent_b = Gents::from_default_agent_documents(
        db_b.node.clone(),
        identity_b.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db_b.home.backend_admission()),
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

    wait_for_enrollment_route_applied(db_a.node.as_ref(), &peer_b, Duration::from_secs(60)).await;
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
                peer_id: "{peer}", node_did: "{did}",
                collections: ["   "], replicator_addresses: ["{addr}"],
                template: "app-collections", source: "test-app-collections",
                created_at: "{now}", updated_at: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = db_a.node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create blank DataPlanePairingDesired: {:?}",
        resp.errors
    );

    tokio::time::sleep(Duration::from_secs(3)).await;

    let after = fetch_pairing_applied(db_a.node.as_ref(), &peer_b)
        .await
        .expect("control applied after soft-skip");
    assert_eq!(
        before, after,
        "control replicator addresses should be unchanged"
    );
    let subscribed = fetch_subscribed_collection_names(db_a.node.as_ref()).await;
    assert!(
        !subscribed.iter().any(|c| c == "ChangeProposed"),
        "blank app-collections must not subscribe ChangeProposed: {subscribed:?}"
    );

    let _ = shutdown_a_tx.send(true);
    let _ = shutdown_b_tx.send(true);
    handle_a.await.unwrap().unwrap();
    handle_b.await.unwrap().unwrap();
    db_a.node.shutdown().await.expect("node shutdown");
    db_b.node.shutdown().await.expect("node shutdown");
}

async fn install_shared_fixture(
    db: &crate::support::TestDb,
    archive: &gents::pack::archive::PackArchive,
) {
    use gents::pack::{
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
    )
    .await
    .unwrap();
    let packs = gents::pack::installed_packs(None, Some((&access, &owner)))
        .await
        .unwrap();
    let pack = packs
        .iter()
        .find(|pack| pack.coordinate == "fixture/shared_fixture")
        .expect("the node records the installed pack");
    assert_eq!(pack.version, "1.0.0");
    assert_eq!(pack.digest, archive.digest());
}

/// The household owner's signed GraphQL principal, which writes and reads its
/// own pack documents.
async fn household_access(db: &crate::support::TestDb) -> gents::ConfigAccess {
    let user = crate::support::load_managed_fixture_user(db).await;
    gents::ConfigAccess::Graphql(
        gents::store::config_client::GraphqlEndpoint::as_principal(
            db.client_endpoint(),
            user.did(),
        )
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
async fn document_pack_collections_stay_local_under_explicit_pairing() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/packs/shared_fixture");
    let (bytes, _) = gents::pack::archive::pack_dir(&path).unwrap();
    let archive = gents::pack::archive::PackArchive::from_bytes(&bytes).unwrap();
    let a = test_p2p_db("shared-fixture-a").await;
    let b = test_p2p_db("shared-fixture-b").await;
    install_shared_fixture(&a, &archive).await;
    install_shared_fixture(&b, &archive).await;

    let mock_a = MockModelEndpoint::start("default").unwrap();
    let mock_b = MockModelEndpoint::start("default").unwrap();
    bind_default_agent_backend(&a, "fixture-a-backend", mock_a.endpoint()).await;
    bind_default_agent_backend(&b, "fixture-b-backend", mock_b.endpoint()).await;
    let options = || DocumentRuntimeOptions {
        tool_ceiling: ToolCeiling::meta_only(),
        ..Default::default()
    };
    let runtime_a = Gents::from_default_agent_documents(
        a.node.clone(),
        a.node_identity.clone(),
        gents::DocumentRuntimeOptions {
            backend_admission: Some(a.home.backend_admission()),
            ..options()
        },
    )
    .await
    .unwrap();
    let runtime_b = Gents::from_default_agent_documents(
        b.node.clone(),
        b.node_identity.clone(),
        gents::DocumentRuntimeOptions {
            backend_admission: Some(b.home.backend_admission()),
            ..options()
        },
    )
    .await
    .unwrap();
    let (stop_a, receiver_a) = tokio::sync::watch::channel(false);
    let (stop_b, receiver_b) = tokio::sync::watch::channel(false);
    let task_a = tokio::spawn(runtime_a.run(receiver_a));
    let task_b = tokio::spawn(runtime_b.run(receiver_b));

    let (peer_a, address_a) = wait_for_peer_identity(a.node.as_ref()).await;
    let (peer_b, address_b) = wait_for_peer_identity(b.node.as_ref()).await;
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
    let control_a =
        wait_for_enrollment_route_applied(a.node.as_ref(), &peer_b, Duration::from_secs(60)).await;
    wait_for_current_route_receipt(
        b.node.as_ref(),
        &enrollment.request_id,
        &enrollment.request_digest,
        &peer_b,
        Duration::from_secs(60),
    )
    .await;
    let control_b =
        wait_for_enrollment_route_applied(b.node.as_ref(), &peer_a, Duration::from_secs(60)).await;
    refuse_app_collection_replication(&a.node, &address_b, "SharedFixtureMove").await;
    refuse_app_collection_replication(&b.node, &address_a, "SharedFixtureMove").await;
    assert_app_collection_refused(a.node.as_ref(), &peer_b, "SharedFixtureMove", &control_a).await;
    assert_app_collection_refused(b.node.as_ref(), &peer_a, "SharedFixtureMove", &control_b).await;

    write_fixture_move(&a, "move-a", "X").await;
    write_fixture_move(&b, "move-b", "O").await;
    assert_stays_local(
        || async {
            fixture_moves(&b, "move-a").await.is_empty()
                && fixture_moves(&a, "move-b").await.is_empty()
        },
        "a pack document replicated to the paired peer",
    )
    .await;
    assert_eq!(fixture_moves(&a, "move-a").await.len(), 1);
    assert_eq!(fixture_moves(&b, "move-b").await.len(), 1);

    let _ = stop_a.send(true);
    let _ = stop_b.send(true);
    task_a.await.unwrap().unwrap();
    task_b.await.unwrap().unwrap();
    a.node.shutdown().await.unwrap();
    b.node.shutdown().await.unwrap();
}
