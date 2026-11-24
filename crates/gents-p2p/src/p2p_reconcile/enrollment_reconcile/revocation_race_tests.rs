use std::sync::Arc;

use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::{KeyIdentity, NodeIdentity};

use super::*;
use crate::p2p_reconcile::embedded_impl::tests::fixtures::{
    runtime_http_test_node, wait_for_active_peer, wait_for_peer_info,
};
use crate::p2p_reconcile::{reconcile_peer_tick, GraphqlPairingStateStore, RemoteP2pAdmin};
use crate::test_support::enrollment::authorize_enrollment_peer;

const ROUTES: &str =
    "{ PeerPairingDesired { _docID peer_id source enrollment_authorization_sequence } }";

fn serve_authority(store: GraphqlEnrollmentStore) -> EnrollmentAuthorityHandle {
    let (mut owner, handle) = enrollment_authority_channel();
    tokio::spawn(async move {
        while let Some(command) = owner.commands.recv().await {
            handle_authority_command(&store, &owner.sender, command).await;
        }
    });
    handle
}

#[tokio::test]
async fn a_route_write_and_a_revocation_of_its_member_are_never_interleaved() {
    let keys = tempfile::tempdir().unwrap();
    let admin: Arc<dyn NodeIdentity> =
        Arc::new(KeyIdentity::load_or_create(keys.path().join("admin.key"), None).unwrap());
    let member: Arc<dyn NodeIdentity> =
        Arc::new(KeyIdentity::load_or_create(keys.path().join("member.key"), None).unwrap());
    let (server_test, _) = runtime_http_test_node(admin.as_ref()).await;
    let (member_test, _) = runtime_http_test_node(member.as_ref()).await;
    let server = &server_test.node;
    let member_node = &member_test.node;
    let server_admin = EmbeddedRemoteP2pAdmin::new(server.clone());
    let member_admin = EmbeddedRemoteP2pAdmin::new(member_node.clone());
    wait_for_peer_info(&member_admin).await;
    let server_addresses = wait_for_peer_info(&server_admin).await;
    member_admin.connect(&server_addresses).await.unwrap();
    wait_for_active_peer(&member_admin).await;
    wait_for_active_peer(&server_admin).await;
    let peer = member_node.p2p().unwrap().local_peer_id().await.unwrap();
    let ticket = member_node
        .p2p()
        .unwrap()
        .shareable_address()
        .await
        .unwrap()
        .unwrap();
    ConfigAccess::local_as(server.clone(), ::identity::Did::new(admin.did()).unwrap())
        .write(
            "test.enrollment.admin_node",
            &format!(
                r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                escape_graphql_string(admin.did())
            ),
        )
        .await
        .unwrap();
    let enrolled = authorize_enrollment_peer(
        server.clone(),
        &format!("network-{}", uuid::Uuid::new_v4()),
        "revocation race",
        admin.clone(),
        member.clone(),
        &peer,
        &ticket,
    )
    .await;
    let store = GraphqlEnrollmentStore::new(server.clone(), admin.clone());
    let before = store.load_projection().await.unwrap();
    assert!(
        exact_authorization_fence(&before, member.did(), &peer)
            .unwrap()
            .is_some(),
        "the member is enrolled before the race"
    );

    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let reconcile = ConfigApplyTxn::with_successful_mutation_pause_at(
        1,
        reached.clone(),
        release.clone(),
        reconcile_data_plane(server, admin.did(), &store, &before),
    );
    tokio::pin!(reconcile);
    tokio::select! {
        () = reached.notified() => {}
        settled = &mut reconcile => panic!("the route write settled before its commit: {settled:?}"),
    }
    assert!(
        gents_store::identity::enrollment_decision_gate(server)
            .try_lock()
            .is_err(),
        "a revocation cannot commit while the route write is open"
    );
    release.notify_one();
    let (reconciled, paused) = reconcile.await;
    reconciled.unwrap();
    assert!(paused);
    let routes = |label: &'static str| {
        let server = server.clone();
        async move {
            let response = ConfigAccess::local(server).execute(ROUTES).await.unwrap();
            response["data"]["PeerPairingDesired"]
                .as_array()
                .unwrap_or_else(|| panic!("{label}: routes omitted"))
                .len()
        }
    };
    assert_eq!(routes("authorized at commit").await, 1);

    store.revoke_request(&enrolled.request_id).await.unwrap();
    let after = store.load_projection().await.unwrap();
    assert!(after.active.is_empty() && after.revoked.len() == 1);

    let pairing = GraphqlPairingStateStore::with_enrollment_authority(
        server.clone(),
        admin.clone(),
        serve_authority(store.clone()),
    );
    let tick = reconcile_peer_tick(&server_admin, &pairing, &peer)
        .await
        .unwrap();
    assert!(tick.ops_applied.is_empty(), "{:?}", tick.ops_applied);
    assert!(server_admin.list_replicators().await.unwrap().is_empty());

    reconcile_data_plane(server, admin.did(), &store, &before)
        .await
        .unwrap();
    assert_eq!(
        routes("a route write on a projection read before the revocation").await,
        0
    );
    member_node.shutdown().await.expect("node shutdown");
    server.shutdown().await.expect("node shutdown");
}
