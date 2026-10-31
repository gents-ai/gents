use std::collections::BTreeSet;
use std::sync::Arc;

use super::materialize_enrollment_desired_in_txn;
use crate::runtime::agent::p2p_reconcile::EnrollmentEndpointEntry;
use crate::store::config_client::ConfigAccess;

async fn fixture() -> Arc<defra_node::EmbeddedNode> {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    // Deliberately omit the unique index: merge-parked twins remain scan-visible
    // even though local creation through the production index would reject them.
    node.add_collection(
        r#"type PeerPairingDesired {
        peer_id: String node_did: String source: String template: String
        collections: [String!] replicator_addresses: [String!] profiles: [String!]
        enrollment_request_digest: String enrollment_authorization_sequence: Int
        enrollment_authorization_expires_at: DateTime created_at: DateTime updated_at: DateTime
    }"#,
    )
    .await
    .unwrap();
    node
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

async fn seed(node: &Arc<defra_node::EmbeddedNode>, peer: &str, ordinal: usize) {
    ConfigAccess::write_local(
        node,
        "test.desired.seed",
        &format!(
            r#"mutation {{ create_PeerPairingDesired(input: {{
        peer_id: "{peer}", node_did: "local", source: "enrollment", template: "client",
        enrollment_request_digest: "old", enrollment_authorization_sequence: 1,
        enrollment_authorization_expires_at: "2099-01-01T00:00:00Z",
        replicator_addresses: ["old-{ordinal}"], collections: null, profiles: null
    }}) {{ _docID }} }}"#
        ),
    )
    .await
    .unwrap();
}

async fn rows(node: &Arc<defra_node::EmbeddedNode>) -> Vec<serde_json::Value> {
    let data = ConfigAccess::local(node.clone())
        .execute(
            r#"{ PeerPairingDesired {
        _docID peer_id node_did source enrollment_request_digest enrollment_authorization_sequence
        enrollment_authorization_expires_at replicator_addresses updated_at
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
    ConfigAccess::write_local(&node, "test.desired.advance_twin", &format!(r#"mutation {{ update_PeerPairingDesired(docID: "{parked}", input: {{ enrollment_request_digest: "verified", enrollment_authorization_sequence: 2 }}) {{ _docID }} }}"#)).await.unwrap();
    let failed = ConfigAccess::write_local(
        &node,
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
        let saved = ConfigAccess::transact_local_write(&node, None, "test.desired.repair", |txn| {
            let entry = &entry;
            Box::pin(async move {
                materialize_enrollment_desired_in_txn(txn, "local", entry, &BTreeSet::new()).await
            })
        })
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
    node.shutdown().await;
}

#[tokio::test]
async fn conflicting_materialization_owners_and_generations_leave_all_rows_unchanged() {
    for patch in [
        r#"node_did: "foreign""#,
        r#"source: "manual""#,
        "enrollment_authorization_sequence: 3",
        r#"enrollment_authorization_sequence: 2, enrollment_request_digest: "conflict""#,
    ] {
        let node = fixture().await;
        seed(&node, "route", 1).await;
        seed(&node, "route", 2).await;
        let id = rows(&node).await[0]["_docID"].as_str().unwrap().to_owned();
        ConfigAccess::write_local(&node, "test.desired.conflict", &format!(r#"mutation {{ update_PeerPairingDesired(docID: "{id}", input: {{ {patch} }}) {{ _docID }} }}"#)).await.unwrap();
        let before = rows(&node).await;
        let entry = authorized();
        assert!(
            ConfigAccess::transact_local_write(&node, None, "test.desired.refuse", |txn| {
                let entry = &entry;
                Box::pin(async move {
                    materialize_enrollment_desired_in_txn(txn, "local", entry, &BTreeSet::new())
                        .await
                })
            })
            .await
            .is_err()
        );
        assert_eq!(rows(&node).await, before);
        node.shutdown().await;
    }
}

#[tokio::test]
async fn expired_authority_rolls_back_the_whole_materialization_transaction() {
    let node = fixture().await;
    let mut entry = authorized();
    entry.authorization_expires_at = "2000-01-01T00:00:00Z".into();
    assert!(ConfigAccess::transact_local_write(&node, None, "test.desired.expired", |txn| {
        let entry = &entry;
        Box::pin(async move {
            txn.execute(r#"mutation { create_PeerPairingDesired(input: {peer_id: "rollback"}) { _docID } }"#).await?;
            materialize_enrollment_desired_in_txn(txn, "local", entry, &BTreeSet::new()).await
        })
    }).await.is_err());
    assert!(rows(&node).await.is_empty());
    node.shutdown().await;
}
