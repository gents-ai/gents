use gents_lean_vocab::tla::replay::{Red, Trace};
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

async fn replay(trace: &Trace) {
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
    let revoke = store.revoke_request(&enrolled.request_id);
    tokio::pin!(revoke);
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
    let pairing = GraphqlPairingStateStore::with_enrollment_authority(
        server.clone(),
        admin.clone(),
        serve_authority(store.clone()),
    );
    let mut committed = false;
    let mut revoked = false;
    let mut waiting = false;
    let mut stale_route = false;
    let mut stale_install = false;
    for step in &trace.steps {
        match step.action.as_str() {
            "RouteRead" => {
                tokio::select! {
                    () = reached.notified() => {}
                    settled = &mut reconcile => panic!("route write settled before commit: {settled:?}"),
                }
            }
            "Revoke" if !committed => {
                assert!(gents_store::identity::enrollment_decision_gate(server)
                    .try_lock()
                    .is_err());
                assert!(futures::poll!(&mut revoke).is_pending());
                waiting = true;
            }
            "Revoke" => {
                (&mut revoke).await.unwrap();
                revoked = true;
            }
            "RouteCommit" => {
                release.notify_one();
                let (result, paused) = (&mut reconcile).await;
                result.unwrap();
                assert!(paused);
                let projection = store.load_projection().await.unwrap();
                stale_route = routes("at commit").await == 1 && projection.active.is_empty();
                assert_eq!(routes("new route").await, 1);
                committed = true;
                if waiting {
                    (&mut revoke).await.unwrap();
                    revoked = true;
                }
            }
            "PeerInstall" => {
                let tick = reconcile_peer_tick(&server_admin, &pairing, &peer)
                    .await
                    .unwrap();
                let installed = !server_admin.list_replicators().await.unwrap().is_empty();
                if revoked {
                    stale_install |= !tick.ops_applied.is_empty() || installed;
                } else {
                    assert!(installed, "enrolled peer installed before revocation");
                }
            }
            "PeerTeardown" => {
                assert!(revoked);
                reconcile_peer_tick(&server_admin, &pairing, &peer)
                    .await
                    .unwrap();
                assert!(server_admin.list_replicators().await.unwrap().is_empty());
            }
            "Sweep" => {
                let current = store.load_projection().await.unwrap();
                reconcile_data_plane(server, admin.did(), &store, &current)
                    .await
                    .unwrap();
                assert_eq!(routes("sweep of revoked route").await, 0);
            }
            action => panic!("unmapped enrollment action {action}"),
        }
    }
    let after = store.load_projection().await.unwrap();
    assert!(revoked && after.active.is_empty() && after.revoked.len() == 1);
    trace.check(
        Red::Defect,
        &[
            ("NoRouteForRevokedMember", !stale_route),
            ("NoInstallForRevokedMember", !stale_install),
        ],
    );

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

#[tokio::test]
async fn enrollment_route_fence_traces() {
    for json in [
        include_str!("../../../../gents/proofs/tla/generated/traces/MCEnrollmentRouteFence.sample-1.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCEnrollmentRouteFenceFilteredRead.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCEnrollmentRouteFencePeerTrustsRoute.json"),
    ] {
        replay(&Trace::load(json, "EnrollmentRouteFence", &[])).await;
    }
}
