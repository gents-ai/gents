mod agent_target;
pub(in crate::runtime::agent::p2p_reconcile) mod fixtures;
mod pairing;
mod peer_admission;
mod replication;
mod seeding;

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};

use p2p::iroh::{IrohDiscoveryConfig, IrohRelayModeConfig};

use super::*;
use crate::defra_node::P2PConfig;
use crate::ensure_runtime_schemas;
use crate::runtime::agent::p2p_reconcile::templates::Scope;
use crate::runtime::agent::p2p_reconcile::GraphqlEnrollmentStore;
use crate::runtime::agent::p2p_reconcile::{combine_filters, equality_filter};
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use defra_p2p_adapter::P2pDocumentRequest;
use fixtures::{runtime_http_test_node, wait_for_active_peer, wait_for_peer_info};
use gents_protocol::enrollment::{
    derive_enrollment_id, encode_offer, enrollment_schema_fingerprint, EnrollmentDecisionKind,
    EnrollmentOfferRecord, EnrollmentRequestRecord, ENROLLMENT_PROTOCOL_VERSION,
};
use gents_protocol::network_token::NetworkRecord;

#[test]
fn native_unauthorized_error_keeps_its_pairing_class() {
    let error = map_p2p_error(
        "sync_branchable_collection",
        P2PError::Unauthorized("denied".into()),
    );
    assert!(matches!(error, RemoteP2pAdminError::RemoteUnauthorized));
    assert_eq!(
        super::super::classify_remote_admin_error(&error),
        super::super::PairingErrorClass::RemoteUnauthorized
    );
}

#[tokio::test]
async fn explicit_file_image_push_refuses_runtime_local_collections() {
    let keys = tempfile::tempdir().unwrap();
    let sender_identity =
        KeyIdentity::load_or_create(keys.path().join("sender.key"), None).unwrap();
    let receiver_identity =
        KeyIdentity::load_or_create(keys.path().join("receiver.key"), None).unwrap();
    let (sender_test, _sender_http) = runtime_http_test_node(&sender_identity).await;
    let (receiver_test, _receiver_http) = runtime_http_test_node(&receiver_identity).await;
    let sender = &sender_test.node;
    let receiver = &receiver_test.node;
    let sender_admin = EmbeddedRemoteP2pAdmin::new(sender.clone());
    let receiver_admin = EmbeddedRemoteP2pAdmin::new(receiver.clone());
    let sender_addresses = wait_for_peer_info(&sender_admin).await;
    let receiver_addresses = wait_for_peer_info(&receiver_admin).await;
    sender_admin.connect(&receiver_addresses).await.unwrap();
    wait_for_active_peer(&sender_admin).await;
    wait_for_active_peer(&receiver_admin).await;

    let refused = receiver_admin
        .add_replicator(
            &sender_addresses,
            &["FileInventoryRequest".into()],
            &PairingFilters::default(),
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(RemoteP2pAdminError::RuntimeLocalCollection(ref name)) if name == "FileInventoryRequest"
        ),
        "{refused:?}"
    );
    let peer_id = receiver.p2p().unwrap().local_peer_id().await.unwrap();
    let pushed = push_documents_to_peer(
        sender,
        &peer_id,
        vec![P2pDocumentRequest {
            collection: "ToolFileChange".into(),
            doc_id: "unselected-image".into(),
        }],
    )
    .await;
    assert!(
        pushed.is_err(),
        "file-change documents reached the P2P transport"
    );
    for node in [sender, receiver] {
        assert!(crate::store::identity::node_p2p(node)
            .unwrap()
            .get_collections()
            .await
            .unwrap()
            .is_empty());
    }

    sender.shutdown().await.expect("node shutdown");
    receiver.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn protocol_allowlist_rejects_local_data_before_transport() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    let admin = EmbeddedRemoteP2pAdmin::new(node);
    for contract in crate::lean_vocab_test::lean_identity_contracts() {
        for name in gents_protocol::schemas::ALL_COLLECTION_NAMES
            .iter()
            .copied()
            .chain(["PackPrivateData"])
        {
            let allowed = contract
                .replicated_protocol_collections
                .iter()
                .any(|collection| collection == name);
            assert_eq!(
                admin
                    .require_protocol_collections(&[name.to_owned()])
                    .is_ok(),
                allowed,
                "{name}"
            );
            // Application collections pass the guard: the overlay owner admits them.
            if allowed || !gents_protocol::peer_schema::is_managed_collection(name) {
                continue;
            }
            let result = admin
                .add_replicator(&[], &[name.to_owned()], &PairingFilters::new())
                .await;
            assert!(
                matches!(result, Err(RemoteP2pAdminError::RuntimeLocalCollection(collection)) if collection == name)
            );
        }
    }
}

/// Every collection a Gents node creates is refused as application data, by
/// name at admission and by name or collection ID at the transport guard, so
/// renaming a request cannot route Gents documents onto a pairing.
#[tokio::test]
async fn every_gents_collection_is_refused_by_name_and_by_id() {
    use crate::runtime::agent::p2p_reconcile::{
        resolve_embedded_collection_id,
        templates::{admit_app_collections, AppCollectionAdmissionError},
    };
    use gents_protocol::peer_schema::AppCollectionRefusal;
    let db = crate::support::test_p2p_db("engineer-p2p-managed-catalog").await;
    let admin = EmbeddedRemoteP2pAdmin::new(db.node.clone());
    let names = db.node.list_collections().unwrap();
    assert!(names.iter().any(|name| name == "AgentSession"), "{names:?}");
    for name in names {
        let admitted =
            admit_app_collections(db.node.clone(), &BTreeSet::from([name.clone()])).await;
        assert!(
            matches!(
                admitted,
                Err(AppCollectionAdmissionError::Refused {
                    refusal: AppCollectionRefusal::Managed,
                    ..
                })
            ),
            "{name}: {admitted:?}"
        );
        let id = resolve_embedded_collection_id(&db.node, &name)
            .unwrap()
            .expect("registered collection ID");
        assert!(
            admit_app_collections(db.node.clone(), &BTreeSet::from([id.clone()]))
                .await
                .is_err(),
            "{name} admitted by collection ID {id}"
        );
        if gents_protocol::schemas::is_replicated_protocol_collection(&name) {
            continue;
        }
        for token in [name.clone(), id] {
            let refused = admin
                .add_replicator(&[], std::slice::from_ref(&token), &PairingFilters::new())
                .await;
            assert!(
                matches!(&refused, Err(RemoteP2pAdminError::RuntimeLocalCollection(n)) if *n == name),
                "{token}: {refused:?}"
            );
        }
    }
}
