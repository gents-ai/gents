//! Renewal is a superseding re-enrollment of the same member: a fresh offer,
//! a request the member signs and an approval at the member's next sequence.
//! The runtime must swap the materialized route to the new generation in place
//! and keep the lease, principal and template otherwise unchanged.

use std::time::{Duration, Instant};

use gents::runtime::agent::p2p_reconcile::{
    enrollment_authority_channel, run_enrollment_reconciler, GraphqlEnrollmentStore,
};
use gents::store::config_client::ConfigAccess;
use gents::store::graphql::escape_graphql_string;
use gents_protocol::enrollment::{
    AuthorizationLease, AuthorizationLeaseState, EnrollmentDecisionKind,
};
use tokio_util::sync::CancellationToken;

use crate::support::enrollment::{authorize_enrollment_generation, EnrollmentGeneration};
use crate::support::{self, TestDb};

struct Stop(CancellationToken);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn enrollment_route(db: &TestDb, peer: &str) -> Vec<serde_json::Value> {
    ConfigAccess::local(db.node.clone())
        .execute(&format!(
            r#"{{ PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{}" }}, source: {{ _eq: "enrollment" }} }}) {{
                template enrollment_request_digest enrollment_authorization_sequence
                enrollment_authorization_expires_at
            }} }}"#,
            escape_graphql_string(peer)
        ))
        .await
        .unwrap()["data"]["PeerPairingDesired"]
        .as_array()
        .unwrap()
        .clone()
}

async fn wait_for_route_sequence(db: &TestDb, peer: &str, sequence: i64) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let rows = enrollment_route(db, peer).await;
        if let [row] = rows.as_slice() {
            if row["enrollment_authorization_sequence"].as_i64() == Some(sequence) {
                return row.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "enrollment route never reached sequence {sequence}: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn re_enrolling_an_active_member_renews_its_lease_in_place() {
    let server = support::test_p2p_db("renewal-server").await;
    let member = support::test_p2p_db("renewal-member").await;
    let member_did = member.node_identity.did().to_string();
    gents::documents::ensure_node(&server.node, server.node_identity.did())
        .await
        .unwrap();
    gents::documents::ensure_node(&member.node, &member_did)
        .await
        .unwrap();
    let (peer, address) = support::enrollment::wait_for_peer_identity(&member.node).await;
    let cancel = CancellationToken::new();
    let _stop = Stop(cancel.clone());
    let (owner, authority) = enrollment_authority_channel();
    let enrollment_task = tokio::spawn(run_enrollment_reconciler(
        server.node.clone(),
        server.node_identity.clone(),
        owner,
        cancel.clone(),
    ));

    // The original enrollment, on a lease that is already inside the window.
    let original = authorize_enrollment_generation(
        server.node.clone(),
        EnrollmentGeneration {
            network_id: "renewal-network",
            network_name: "Renewal network",
            label: "",
            lease: Duration::from_secs(3 * 24 * 60 * 60),
        },
        server.node_identity.clone(),
        member.node_identity.clone(),
        &peer,
        &address,
    )
    .await;
    let first = authority
        .fresh_authorization(&member_did, &peer)
        .await
        .unwrap()
        .expect("original generation is current");
    assert_eq!(first.authorization_sequence, 1);
    assert_eq!(first.request_id, original.request_id);
    let first_lease = AuthorizationLease::at(&first.authorization_expires_at, chrono::Utc::now());
    assert_eq!(first_lease.state, AuthorizationLeaseState::RenewalDue);
    let first_route = wait_for_route_sequence(&server, &peer, 1).await;
    assert_eq!(
        first_route["enrollment_authorization_expires_at"].as_str(),
        Some(first.authorization_expires_at.as_str())
    );

    // Renewal: the same member, peer and network re-enroll with a fresh offer.
    let renewal = authorize_enrollment_generation(
        server.node.clone(),
        EnrollmentGeneration {
            network_id: "renewal-network",
            network_name: "Renewal network",
            label: "-renewal-1",
            lease: Duration::from_secs(
                gents_protocol::enrollment::DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
            ),
        },
        server.node_identity.clone(),
        member.node_identity.clone(),
        &peer,
        &address,
    )
    .await;
    assert_ne!(renewal.request_id, original.request_id);

    let renewed = authority
        .fresh_authorization(&member_did, &peer)
        .await
        .unwrap()
        .expect("renewed generation is current");
    assert_eq!(renewed.authorization_sequence, 2);
    assert_eq!(renewed.request_id, renewal.request_id);
    assert!(renewed.authorization_expires_at > first.authorization_expires_at);
    // No widening: the same principal, transport peer, owner, network and profile.
    assert_eq!(renewed.member_did, first.member_did);
    assert_eq!(renewed.member_peer, first.member_peer);
    assert_eq!(renewed.owner_node, first.owner_node);
    assert_eq!(renewed.network_id, first.network_id);
    assert_eq!(renewed.admin_did, first.admin_did);
    assert_eq!(renewed.profile, first.profile);
    let renewed_lease =
        AuthorizationLease::at(&renewed.authorization_expires_at, chrono::Utc::now());
    assert_eq!(renewed_lease.state, AuthorizationLeaseState::Current);
    assert!(renewed_lease.remaining_days() >= 29);

    // The superseded generation is gone from the projection, and the member
    // DID and peer each resolve to exactly the new one.
    let store = GraphqlEnrollmentStore::new(server.node.clone(), server.node_identity.clone());
    let projection = store.load_projection().await.unwrap();
    assert!(projection.conflict.is_none());
    let active = projection
        .active
        .iter()
        .filter(|active| active.request.candidate_did == member_did)
        .collect::<Vec<_>>();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].request.request_id, renewal.request_id);
    assert_eq!(
        authority
            .fresh_member_authorization(&member_did)
            .await
            .unwrap()
            .map(|fence| fence.authorization_sequence),
        Some(2)
    );
    assert_eq!(
        authority
            .fresh_peer_authorization(&peer)
            .await
            .unwrap()
            .map(|fence| fence.authorization_sequence),
        Some(2)
    );

    // The one enrollment route is rewritten in place to the renewed generation.
    let route = wait_for_route_sequence(&server, &peer, 2).await;
    assert_eq!(route["template"].as_str(), Some("client"));
    assert_eq!(
        route["enrollment_request_digest"].as_str(),
        Some(renewal.request_digest.as_str())
    );
    assert_eq!(
        route["enrollment_authorization_expires_at"].as_str(),
        Some(renewed.authorization_expires_at.as_str())
    );

    // Approving the renewal again is idempotent: it reports the existing
    // approval and signs no further generation.
    let repeated = store
        .decide_request(&renewal.request_id, EnrollmentDecisionKind::Approved)
        .await
        .expect("repeated approval reports the existing generation");
    assert_eq!(repeated.state, "approved");
    assert_eq!(repeated.request_id, renewal.request_id);
    let next = store.load_projection().await.unwrap();
    assert_eq!(
        next.next_authorization_sequences,
        projection.next_authorization_sequences
    );
    let after = authority
        .fresh_authorization(&member_did, &peer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after, renewed);
    assert_eq!(enrollment_route(&server, &peer).await.len(), 1);

    cancel.cancel();
    enrollment_task.await.unwrap().unwrap();
}
