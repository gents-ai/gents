use gents_protocol::operations::p2p::{RegistryError, RegistryRegistration};
use gents_store::config_client::ConfigAccess;

use super::fixtures::runtime_test_node;
use super::*;
use crate::operations::{pairing, registry};

#[tokio::test]
async fn a_node_registers_lists_and_deregisters_itself_through_the_handlers() {
    let test = runtime_test_node().await;
    let access = ConfigAccess::local(Arc::clone(&test.node));
    let now = chrono::Utc::now();
    let registration = RegistryRegistration {
        peer_id: Some("peer-one".to_owned()),
        node_did: "did:key:node-one".to_owned(),
        listen_addresses: vec!["/ip4/127.0.0.1/udp/4001/quic-v1".to_owned()],
        display_name: Some(" Studio ".to_owned()),
        templates: vec!["machine".to_owned(), "machine".to_owned()],
        network_id: None,
    };

    let unknown = registry::register_self(
        &access,
        &RegistryRegistration {
            templates: vec!["no-such-template".to_owned()],
            ..registration.clone()
        },
        now,
    )
    .await
    .expect_err("an unknown template is refused");
    assert!(
        matches!(unknown, RegistryError::UnknownTemplate(_)),
        "{unknown}"
    );
    let stopped = registry::register_self(
        &access,
        &RegistryRegistration {
            peer_id: None,
            ..registration.clone()
        },
        now,
    )
    .await
    .expect_err("a runtime that reports no peer ID is refused");
    assert!(matches!(stopped, RegistryError::PeerIdUnknown), "{stopped}");

    let registered = registry::register_self(&access, &registration, now)
        .await
        .expect("register");
    assert_eq!(registered.templates, ["machine"]);
    assert_eq!(registered.display_name.as_deref(), Some("Studio"));
    assert_eq!(
        registered.addresses,
        ["/ip4/127.0.0.1/udp/4001/quic-v1/p2p/peer-one"]
    );

    let peers = registry::list_registered_peers(&access, now)
        .await
        .expect("list");
    let [peer] = peers.as_slice() else {
        panic!("one registered peer, found {peers:?}")
    };
    assert_eq!(peer.peer_id, "peer-one");
    assert!(peer.online && !peer.paired, "{peer:?}");
    let later = now + chrono::Duration::hours(1);
    let stale = registry::list_registered_peers(&access, later)
        .await
        .expect("list later");
    assert!(!stale[0].online, "{stale:?}");

    assert!(pairing::list_pairings(&access, &[], now)
        .await
        .expect("pairings")
        .is_empty());
    let targets = pairing::pairing_reader_targets(&access)
        .await
        .expect("reader targets");
    assert!(targets.desired.is_empty() && targets.applied.is_empty());

    let removed = registry::deregister_self(&access, Some("peer-one"))
        .await
        .expect("deregister");
    assert_eq!(removed.removed_count, 1);
    assert!(registry::list_registered_peers(&access, now)
        .await
        .expect("list after removal")
        .is_empty());
}
