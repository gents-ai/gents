use std::collections::BTreeSet;
use std::sync::Arc;

use super::materialize_enrollment_desired_in_txn;
use crate::runtime::agent::p2p_reconcile::EnrollmentEndpointEntry;
use crate::store::config_client::ConfigAccess;

async fn fixture() -> Arc<defra_node::Node> {
    use crate::NodeIdentity;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime");
    let runtime = crate::KeyIdentity::load_or_create(&path, None).unwrap();
    let key = crate::store::identity::load_file_identity(&path)
        .unwrap()
        .private_key_bytes();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(key))
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    node.add_dac_policy_with_counter_as(
        &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await
    .unwrap();
    node.add_collection(gents_protocol::schemas::NODE)
        .await
        .unwrap();
    // Deliberately omit the unique index: merge-parked twins remain scan-visible
    // even though local creation through the production index would reject them.
    node.add_collection(
        r#"type PeerPairingDesired @policy(id: "202ce2df1b18d27183b29b653e2c06654178b2e79d4d6b93229d0c706380e71e", resource: "pairing_observations") {
        peer_id: String node_did: String actor_did: String @immutable source: String template: String
        collections: [String!] replicator_addresses: [String!] profiles: [String!]
        enrollment_request_digest: String enrollment_authorization_sequence: Int
        enrollment_authorization_expires_at: DateTime created_at: DateTime updated_at: DateTime
        node_doc_id: String parent_linked: Boolean
    }"#,
    )
    .await
    .unwrap();
    node
}

fn runtime_access(node: &Arc<defra_node::Node>) -> ConfigAccess {
    ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node.node_identity_did().unwrap()).unwrap(),
    )
}

fn runtime_actor(node: &Arc<defra_node::Node>) -> Option<identity::Did> {
    Some(identity::Did::new(node.node_identity_did().unwrap()).unwrap())
}

fn authorized() -> EnrollmentEndpointEntry {
    EnrollmentEndpointEntry {
        desired_id: "route".into(),
        peer_id: "peer".into(),
        node_did: "remote".into(),
        address: "fresh-address".into(),
        request_digest: "verified".into(),
        authorization_sequence: 2,
        authorization_expires_at: "2099-01-01T00:00:00Z".into(),
    }
}

async fn seed(node: &Arc<defra_node::Node>, peer: &str, ordinal: usize) {
    runtime_access(node).write(
        "test.desired.seed",
        &format!(
            r#"mutation {{ create_PeerPairingDesired(input: {{
        peer_id: "{}", node_did: "local", actor_did: "{}", source: "enrollment", template: "client", parent_linked: false,
        enrollment_request_digest: "old", enrollment_authorization_sequence: 1,
        enrollment_authorization_expires_at: "2099-01-01T00:00:00Z",
        replicator_addresses: ["old-{ordinal}"], collections: null, profiles: null
    }}) {{ _docID }} }}"#, crate::store::graphql::escape_graphql_string(peer), crate::store::graphql::escape_graphql_string(node.node_identity_did().unwrap())
        ),
    )
    .await
    .unwrap();
}

async fn rows(node: &Arc<defra_node::Node>) -> Vec<serde_json::Value> {
    let data = runtime_access(node)
        .execute(
            r#"{ PeerPairingDesired {
        _docID peer_id node_did source enrollment_request_digest enrollment_authorization_sequence
        enrollment_authorization_expires_at replicator_addresses updated_at parent_linked
    } }"#,
        )
        .await
        .unwrap();
    serde_json::from_value(data["data"]["PeerPairingDesired"].clone()).unwrap()
}

#[tokio::test]
async fn native_twins_reject_filter_upsert_but_repair_the_canonical_witness() {
    let node = fixture().await;
    seed(&node, "route", 1).await;
    seed(&node, "route", 2).await;
    seed(&node, "other", 3).await;
    let before = rows(&node).await;
    let mut twins = before
        .iter()
        .filter(|row| row["peer_id"] == "route")
        .collect::<Vec<_>>();
    twins.sort_by_key(|row| row["_docID"].as_str().unwrap());
    let canonical = twins[0]["_docID"].as_str().unwrap().to_owned();
    let parked = twins[1]["_docID"].as_str().unwrap().to_owned();
    runtime_access(&node).write("test.desired.advance_twin", &format!(r#"mutation {{ update_PeerPairingDesired(docID: "{parked}", input: {{ enrollment_request_digest: "verified", enrollment_authorization_sequence: 2 }}) {{ _docID }} }}"#)).await.unwrap();
    let failed = runtime_access(&node)
        .write(
            "test.desired.old_upsert",
            r#"mutation {
        upsert_PeerPairingDesired(filter: {peer_id: {_eq: "route"}},
        add: {peer_id: "route"}, update: {template: "client"}) { _docID }
    }"#,
        )
        .await
        .unwrap_err();
    assert!(format!("{failed:#}").contains("multiple matching documents"));
    let mut first_repair = None;
    for _ in 0..2 {
        let entry = authorized();
        let saved = ConfigAccess::transact_local_write(
            &node,
            runtime_actor(&node),
            "test.desired.repair",
            |txn| {
                let entry = &entry;
                Box::pin(async move {
                    materialize_enrollment_desired_in_txn(txn, "local", entry, &BTreeSet::new())
                        .await
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(saved, canonical);
        let current = rows(&node).await;
        if let Some(previous) = &first_repair {
            assert_eq!(
                &current, previous,
                "restart is idempotent, including timestamps"
            );
        }
        first_repair = Some(current);
    }
    let after = rows(&node).await;
    assert_eq!(
        after.len(),
        3,
        "parked twins and unrelated rows remain intact"
    );
    let repaired = after.iter().find(|row| row["_docID"] == canonical).unwrap();
    assert_eq!(repaired["enrollment_authorization_sequence"], 2);
    assert_eq!(
        repaired["replicator_addresses"],
        serde_json::json!(["fresh-address"])
    );
    assert_eq!(
        after.iter().find(|row| row["peer_id"] == "other"),
        before.iter().find(|row| row["peer_id"] == "other")
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn conflicting_materialization_owners_leave_all_rows_unchanged() {
    for patch in [r#"node_did: "foreign""#, r#"source: "manual""#] {
        let node = fixture().await;
        seed(&node, "route", 1).await;
        seed(&node, "route", 2).await;
        let id = rows(&node).await[0]["_docID"].as_str().unwrap().to_owned();
        runtime_access(&node).write("test.desired.conflict", &format!(r#"mutation {{ update_PeerPairingDesired(docID: "{id}", input: {{ {patch} }}) {{ _docID }} }}"#)).await.unwrap();
        let before = rows(&node).await;
        let entry = authorized();
        assert!(ConfigAccess::transact_local_write(
            &node,
            runtime_actor(&node),
            "test.desired.refuse",
            |txn| {
                let entry = &entry;
                Box::pin(async move {
                    materialize_enrollment_desired_in_txn(txn, "local", entry, &BTreeSet::new())
                        .await
                })
            }
        )
        .await
        .is_err());
        assert_eq!(rows(&node).await, before);
        node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn expired_authority_rolls_back_the_whole_materialization_transaction() {
    let node = fixture().await;
    let mut entry = authorized();
    entry.authorization_expires_at = "2000-01-01T00:00:00Z".into();
    assert!(ConfigAccess::transact_local_write(&node, runtime_actor(&node), "test.desired.expired", |txn| {
        let entry = &entry;
        Box::pin(async move {
            txn.execute(r#"mutation { create_PeerPairingDesired(input: {peer_id: "rollback"}) { _docID } }"#).await?;
            materialize_enrollment_desired_in_txn(txn, "local", entry, &BTreeSet::new()).await
        })
    }).await.is_err());
    assert!(rows(&node).await.is_empty());
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn client_owned_route_can_name_a_remote_node_without_a_local_node_document() {
    use crate::NodeIdentity;
    let model = &crate::lean_vocab_test::lean_contract_snapshot().identity_desired_publisher_cases;
    let allowed = model.iter().find(|case| case["name"] == "client").unwrap()["allowed"]
        .as_bool()
        .unwrap();
    let rejected = model.iter().find(|case| case["name"] == "unheld").unwrap()["allowed"]
        .as_bool()
        .unwrap();
    let node = fixture().await;
    let directory = tempfile::tempdir().unwrap();
    let remote = crate::KeyIdentity::load_or_create(directory.path().join("remote"), None).unwrap();
    let local = node.node_identity_did().unwrap().to_owned();
    assert_ne!(local, remote.did());
    let entry = authorized();
    let saved = ConfigAccess::transact_local_write(
        &node,
        runtime_actor(&node),
        "test.desired.client",
        |txn| {
            let entry = &entry;
            let target = remote.did();
            Box::pin(async move {
                materialize_enrollment_desired_in_txn(txn, target, entry, &BTreeSet::new()).await
            })
        },
    )
    .await;
    assert_eq!(saved.is_ok(), allowed);
    let doc = saved.unwrap();
    let result = rows(&node).await;
    let row = result.iter().find(|row| row["_docID"] == doc).unwrap();
    assert_eq!(row["node_did"], remote.did());
    let before = crate::lean_vocab_test::lean_contract_snapshot()
        .identity_deferred_operation_cases
        .iter()
        .find(|case| case["name"] == "before-node")
        .unwrap();
    assert_eq!(row["parent_linked"], before["expected_linked"]);
    let owners = runtime_access(&node)
        .transact_readonly("test.desired.owner", |txn| {
            let doc = &doc;
            Box::pin(async move {
                txn.document_owners("PeerPairingDesired", std::slice::from_ref(doc))
                    .await
            })
        })
        .await
        .unwrap();
    assert_eq!(
        owners[0].owner.as_ref().map(identity::Did::as_str),
        Some(local.as_str())
    );
    let actor = identity::Did::new(remote.did()).unwrap();
    let denied =
        ConfigAccess::transact_local_write(&node, Some(actor), "test.desired.unheld", |txn| {
            let entry = &entry;
            let target = remote.did();
            Box::pin(async move {
                materialize_enrollment_desired_in_txn(txn, target, entry, &BTreeSet::new()).await
            })
        })
        .await;
    assert_eq!(denied.is_ok(), rejected);
    let denied = denied.unwrap_err();
    assert!(
        gents_protocol::error_chain::find_map_source(denied.as_ref(), |cause| cause
            .downcast_ref::<crate::store::config_client::ConfigClientError>()
            .map(|error| matches!(
                error,
                crate::store::config_client::ConfigClientError::ManagedAgentSignerRequired
            )))
        .unwrap_or(false),
        "{denied:#}"
    );
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn cached_generations_cannot_veto_the_current_verified_enrollment_request() {
    for (cached_digest, cached_sequence, cached_expiry) in [
        ("previous-request", 1, "2099-01-01T00:00:00Z"),
        ("previous-request", 99, "2099-01-01T00:00:00Z"),
        ("verified", 1, "2000-01-01T00:00:00Z"),
    ] {
        let node = fixture().await;
        seed(&node, "route", 1).await;
        seed(&node, "route", 2).await;
        let before = rows(&node).await;
        let mut twins = before.iter().collect::<Vec<_>>();
        twins.sort_by_key(|row| row["_docID"].as_str().unwrap());
        let canonical = twins[0]["_docID"].as_str().unwrap().to_owned();
        let parked = twins[1]["_docID"].as_str().unwrap().to_owned();
        // Same owner/source and equal counters do not imply the same request.
        // Keep all other fields current so the no-op path must compare the
        // complete verified generation, not just its counter or endpoint.
        runtime_access(&node).write("test.desired.cached_generations", &format!(r#"mutation {{
            stale: update_PeerPairingDesired(docID: "{canonical}", input: {{
                enrollment_request_digest: "{cached_digest}", enrollment_authorization_sequence: {cached_sequence},
                enrollment_authorization_expires_at: "{cached_expiry}", replicator_addresses: ["fresh-address"]
            }}) {{ _docID }}
            parked: update_PeerPairingDesired(docID: "{parked}", input: {{
                enrollment_request_digest: "verified", enrollment_authorization_sequence: 1
            }}) {{ _docID }}
        }}"#)).await.unwrap();
        let parked_before = rows(&node)
            .await
            .into_iter()
            .find(|row| row["_docID"] == parked)
            .unwrap();
        let mut current = authorized();
        current.authorization_sequence = 1;
        let saved = ConfigAccess::transact_local_write(
            &node,
            runtime_actor(&node),
            "test.desired.current_request",
            |txn| {
                let current = &current;
                Box::pin(async move {
                    materialize_enrollment_desired_in_txn(txn, "local", current, &BTreeSet::new())
                        .await
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(saved, canonical);
        let after = rows(&node).await;
        assert_eq!(after.len(), 2);
        let repaired = after.iter().find(|row| row["_docID"] == canonical).unwrap();
        assert_eq!(repaired["enrollment_request_digest"], "verified");
        assert_eq!(repaired["enrollment_authorization_sequence"], 1);
        assert_eq!(
            repaired["enrollment_authorization_expires_at"],
            "2099-01-01T00:00:00Z"
        );
        assert_eq!(
            after.iter().find(|row| row["_docID"] == parked),
            Some(&parked_before)
        );
        node.shutdown().await.expect("node shutdown");
    }
}
