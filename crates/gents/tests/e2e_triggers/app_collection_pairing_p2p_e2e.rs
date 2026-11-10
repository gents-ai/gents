use std::time::{Duration, Instant};

use gents::defra_node::Node as EmbeddedNode;
use gents::inference::llm::tool::Tool;
use gents::runtime::agent::p2p_reconcile::{
    EmbeddedRemoteP2pAdmin, GraphqlEnrollmentStore, RemoteP2pAdmin,
};
use gents::store::config_client::NodePermission;
use gents::store::graphql::escape_graphql_string;
use gents::tools::builtin::p2p::P2pTool;
use gents::{DocumentRuntimeOptions, Gents, NodeIdentity, ToolCeiling};
use serde_json::{json, Value};

use crate::support::enrollment::{authorize_enrollment_peer, wait_for_peer_identity};
use crate::support::fixtures::bind_default_agent_backend;
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::{install_managed_agent, load_managed_fixture_user, test_p2p_db, TestDb};

const NETWORK_ID: &str = "net-app-collection-pairing";
const NETWORK_NAME: &str = "App Collection Pairing Net";

async fn register_change_proposed_schema(node: &EmbeddedNode) {
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

async fn write_change_note(node: &EmbeddedNode, text: &str) {
    let text = escape_graphql_string(text);
    let response = node
        .execute(&format!(
            r#"mutation {{ create_ChangeNote(input: {{ text: "{text}" }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "create_ChangeNote failed: {:?}",
        response.errors
    );
}

async fn change_note_texts(node: &EmbeddedNode) -> Vec<String> {
    let response = node.execute("{ ChangeNote { text } }").await;
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

async fn change_ids(node: &EmbeddedNode) -> Vec<String> {
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

async fn wait_until<F, Fut>(what: &str, condition: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(45);
    while !condition().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_change(node: &EmbeddedNode, external_id: &str) {
    wait_until(&format!("ChangeProposed {external_id}"), || async {
        change_ids(node).await.iter().any(|id| id == external_id)
    })
    .await;
}

async fn replicator_carries(
    node: &std::sync::Arc<EmbeddedNode>,
    peer: &str,
    collection: &str,
) -> bool {
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

/// The Agent that runs the `p2p` tool. Its overlay delete uses the node
/// permission the Tools projection grants for `p2p_mutate`; the projection
/// itself is pinned by `covered_tool_operations_follow_executable_identity_model`.
async fn pairing_agent(db: &TestDb) -> std::sync::Arc<dyn NodeIdentity> {
    let user = load_managed_fixture_user(db).await;
    let access = db.client_access(&user);
    let (agent, _) = install_managed_agent(db, &access, &user, "Pairing agent").await;
    gents::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).expect("runtime DID"),
    )
    .grant_node_permission(
        &identity::Did::new(agent.did()).expect("agent DID"),
        NodePermission::DocumentDelete,
    )
    .await
    .expect("grant p2p pairing delete");
    agent
}

fn pairing_tool(
    db: &TestDb,
    agent: Option<std::sync::Arc<dyn NodeIdentity>>,
    mutate: bool,
) -> P2pTool {
    P2pTool::new(
        db.node.clone(),
        Some(db.node_identity.clone()),
        agent,
        mutate,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_collections_replicate_only_after_each_node_opts_in() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    std::env::set_var("GENTS_PAIRING_SWEEP_MS", "1000");

    let db_a = test_p2p_db("app-collection-pairing-a").await;
    let db_b = test_p2p_db("app-collection-pairing-b").await;
    for db in [&db_a, &db_b] {
        register_change_proposed_schema(db.node.as_ref()).await;
        // A collection without @branchable replicates through the same route.
        db.node
            .add_collection("type ChangeNote { text: String }")
            .await
            .expect("add_collection ChangeNote");
        db.node
            .add_collection(&format!(
                r#"type GuardedChange @policy(id: "{}", resource: "configuration") {{ text: String }}"#,
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
            ))
            .await
            .expect("add_collection GuardedChange");
    }

    let identity_a = db_a.node_identity.clone();
    let identity_b = db_b.node_identity.clone();

    let mock_a = MockModelEndpoint::start("default").unwrap();
    let mock_b = MockModelEndpoint::start("default").unwrap();
    bind_default_agent_backend(&db_a, "backend-app-collection-a", mock_a.endpoint()).await;
    bind_default_agent_backend(&db_b, "backend-app-collection-b", mock_b.endpoint()).await;

    let opting_agent_a = pairing_agent(&db_a).await;
    let opting_agent_b = pairing_agent(&db_b).await;
    let tool_a = pairing_tool(&db_a, Some(opting_agent_a.clone()), true);
    let tool_b = pairing_tool(&db_b, Some(opting_agent_b), true);
    let other_agent = pairing_agent(&db_a).await;
    let other_agent_tool = pairing_tool(&db_a, Some(other_agent.clone()), true);

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
    wait_for_enrollment_route_applied(db_a.node.as_ref(), &peer_b, Duration::from_secs(60)).await;
    wait_for_enrollment_route_applied(db_b.node.as_ref(), &peer_a, Duration::from_secs(60)).await;

    // Enrollment alone starts no sync.
    write_change_proposed(db_a.node.as_ref(), "before-opt-in", "signup").await;
    assert_stays_local(
        || async { query_change_proposed(db_b.node.as_ref()).await.is_empty() },
        "enrollment alone replicated an application document",
    )
    .await;
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

    // A opts in toward B: A's data reaches B, including writes from before the
    // opt-in, and the opt-in covers only this direction.
    let applied = p2p_call(
        &tool_a,
        pairing("pairings_apply", &["ChangeProposed", "ChangeNote"]),
    )
    .await
    .expect("apply pairing");
    assert_eq!(applied["outcome"]["unchanged"], false);
    assert_eq!(applied["next_call"]["action"], "pairings_get");
    wait_for_change(db_b.node.as_ref(), "before-opt-in").await;
    write_change_proposed(db_a.node.as_ref(), "after-opt-in", "signup").await;
    write_change_note(db_a.node.as_ref(), "non-branchable note").await;
    wait_for_change(db_b.node.as_ref(), "after-opt-in").await;
    wait_until("the non-branchable note", || async {
        change_note_texts(db_b.node.as_ref())
            .await
            .iter()
            .any(|text| text == "non-branchable note")
    })
    .await;
    assert!(
        replicator_carries(&db_a.node, &peer_b, "ChangeProposed").await,
        "the applied route must carry the application collection"
    );
    let retried = p2p_call(
        &tool_a,
        pairing("pairings_apply", &["ChangeProposed", "ChangeNote"]),
    )
    .await
    .expect("retry apply pairing");
    assert_eq!(retried["outcome"]["unchanged"], true);
    // The overlay belongs to the Agent that wrote it; ACP keeps a second Agent out.
    for call in [
        pairing("pairings_apply", &["ChangeProposed"]),
        json!({"action":"pairings_revoke","peer_id":peer_b}),
    ] {
        let refused = p2p_call(&other_agent_tool, call).await.unwrap_err();
        assert!(refused.contains("managed by another owner"), "{refused}");
    }
    // Native ACP, not only the tool's check, keeps the sibling out: it holds the
    // same delete grant, yet deleting the overlay directly as itself removes nothing.
    let overlay = p2p_call(&tool_a, json!({"action":"pairings_get","peer_id":peer_b}))
        .await
        .unwrap();
    assert_eq!(
        overlay["outcome"]["desired"].as_array().map(Vec::len),
        Some(1)
    );
    let sibling = gents::ConfigAccess::local_managed_agent(
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
        .await;
    assert!(
        !deleted
            .as_ref()
            .is_ok_and(|value| value["data"]["delete_DataPlanePairingDesired"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())),
        "{deleted:?}"
    );
    let kept = p2p_call(&tool_a, json!({"action":"pairings_get","peer_id":peer_b}))
        .await
        .unwrap();
    assert_eq!(kept["outcome"]["desired"], overlay["outcome"]["desired"]);

    write_change_proposed(db_b.node.as_ref(), "b-before-its-opt-in", "signup").await;
    assert_stays_local(
        || async {
            !change_ids(db_a.node.as_ref())
                .await
                .contains(&"b-before-its-opt-in".to_owned())
        },
        "a node that never opted in pushed its application documents",
    )
    .await;

    // Each node opts in on its own initiative; B's opt-in carries B's data.
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
    wait_for_change(db_a.node.as_ref(), "b-before-its-opt-in").await;
    write_change_proposed(db_a.node.as_ref(), "both-a", "signup").await;
    write_change_proposed(db_b.node.as_ref(), "both-b", "signup").await;
    wait_for_change(db_b.node.as_ref(), "both-a").await;
    wait_for_change(db_a.node.as_ref(), "both-b").await;

    // Revoking stops new data; enrollment and the peer's own opt-in remain.
    let revoked = p2p_call(
        &tool_a,
        json!({"action":"pairings_revoke","peer_id":peer_b}),
    )
    .await
    .expect("revoke pairing");
    assert_eq!(revoked["outcome"]["unchanged"], false);
    wait_until(
        "the route to stop carrying the application collection",
        || async { !replicator_carries(&db_a.node, &peer_b, "ChangeProposed").await },
    )
    .await;
    write_change_proposed(db_a.node.as_ref(), "after-revoke", "signup").await;
    assert_stays_local(
        || async {
            !change_ids(db_b.node.as_ref())
                .await
                .contains(&"after-revoke".to_owned())
        },
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
    write_change_proposed(db_b.node.as_ref(), "b-still-flows", "signup").await;
    wait_for_change(db_a.node.as_ref(), "b-still-flows").await;

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
