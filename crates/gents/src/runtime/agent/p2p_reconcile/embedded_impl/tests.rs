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
use crate::runtime::agent::p2p_reconcile::templates::{
    Scope, AGENT_TARGET_CALLER_TEMPLATE, AGENT_TARGET_HOST_TEMPLATE,
};
use crate::runtime::agent::p2p_reconcile::GraphqlEnrollmentStore;
use crate::runtime::agent::p2p_reconcile::{combine_filters, equality_filter};
use crate::runtime::agent::p2p_reconcile::resolve_template;
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::{KeyIdentity, NodeIdentity};
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
            &["FileImageHydrationRequest".into()],
            &PairingFilters::default(),
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(RemoteP2pAdminError::RuntimeLocalCollection(ref name)) if name == "FileImageHydrationRequest"
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
    assert!(sender_admin
        .list_p2p_collections()
        .await
        .unwrap()
        .is_empty());
    assert!(receiver_admin
        .list_p2p_collections()
        .await
        .unwrap()
        .is_empty());

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
            if allowed {
                continue;
            }
            for result in [
                admin.add_p2p_collections(&[name.to_owned()]).await,
                admin
                    .add_replicator(&[], &[name.to_owned()], &PairingFilters::new())
                    .await,
                admin.sync_documents(name, &[], None).await,
                admin.sync_branchable_collection(name, None).await,
            ] {
                assert!(
                    matches!(result, Err(RemoteP2pAdminError::RuntimeLocalCollection(collection)) if collection == name)
                );
            }
        }
    }
}
