//! Renewal is a superseding re-enrollment of the same member: a fresh offer,
//! a request the member signs and an approval at the member's next sequence.
//! The runtime must swap the materialized route to the new generation in place
//! and keep the lease, principal and template otherwise unchanged.

use std::time::{Duration, Instant};

use gents::runtime::agent::p2p_reconcile::{
    enrollment_authority_channel, run_enrollment_reconciler, run_pairing_reconciler,
    EnrollmentRenewalPolicy, GraphqlEnrollmentStore,
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
async fn opted_in_members_renew_over_authenticated_routes() {
    use gents::runtime::agent::p2p_reconcile::enrollment_candidate::request_enrollment;
    use gents::runtime::agent::p2p_reconcile::enrollment_member_authority::{
        load_status_enrollment_approvals, EnrollmentAuthorityOutcome,
    };

    let snapshot: serde_json::Value =
        serde_json::from_str(&gents_lean_contract::load_contract_json().unwrap()).unwrap();
    let modeled = |name: &str| {
        snapshot["enrollment_renewal_intent_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()["allowed"]
            .as_bool()
            .unwrap()
    };
    let server = support::test_p2p_db("automatic-renewal-server").await;
    let member = support::test_p2p_db("automatic-renewal-member").await;
    gents::documents::ensure_node(&server.node, server.node_identity.did())
        .await
        .unwrap();
    gents::documents::ensure_node(&member.node, member.node_identity.did())
        .await
        .unwrap();
    let (member_peer, _) = support::enrollment::wait_for_peer_identity(&member.node).await;
    let (server_peer, _) = support::enrollment::wait_for_peer_identity(&server.node).await;
    let (_, token) = support::enrollment::signed_enrollment_offer(
        &server.node,
        "automatic-renewal-network",
        "Renewal network",
        server.node_identity.as_ref(),
        "automatic-renewal-bootstrap".into(),
    )
    .await;
    let first = request_enrollment(&member.node, member.node_identity.as_ref(), &token)
        .await
        .unwrap();
    let store = GraphqlEnrollmentStore::new(server.node.clone(), server.node_identity.clone());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if store
            .load_projection()
            .await
            .unwrap()
            .pending
            .iter()
            .any(|pending| pending.request.request_id == first.request_id)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "initial enrollment request did not arrive"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    store
        .decide_request_with_lease(
            &first.request_id,
            EnrollmentDecisionKind::Approved,
            Duration::from_secs(120),
        )
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let mut forged = gents_protocol::enrollment::RenewalIntent {
        network_id: first.network_id.clone(),
        admin_did: first.admin_did.clone(),
        server_peer: first.server_peer.clone(),
        member_did: first.candidate_did.clone(),
        member_peer: first.candidate_peer.clone(),
        predecessor_sequence: 1,
        issued_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        expires_at: (now + chrono::Duration::seconds(60))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        nonce: "invalid-signature".into(),
        member_sig: vec![0; 64],
    };
    let disabled_result = store.issue_renewal_offer(&forged).await;
    assert_eq!(disabled_result.is_ok(), modeled("disabled"));
    let disabled = disabled_result.unwrap_err();
    assert!(disabled
        .to_string()
        .contains("automatic enrollment renewal is disabled"));
    store
        .set_renewal_policy(EnrollmentRenewalPolicy {
            enabled: true,
            lease_seconds: None,
        })
        .await
        .unwrap();
    let signature_result = store.issue_renewal_offer(&forged).await;
    assert_eq!(signature_result.is_ok(), modeled("unsigned"));
    let invalid_signature = signature_result.unwrap_err();
    assert!(format!("{invalid_signature:#}").contains("signature verification failed"));
    forged.member_sig = member
        .node_identity
        .sign(&forged.signing_payload())
        .await
        .unwrap();
    forged.member_peer = "another-peer".into();
    assert_eq!(
        store.issue_renewal_offer(&forged).await.is_ok(),
        modeled("wrong_peer")
    );
    let cancel = CancellationToken::new();
    let _stop = Stop(cancel.clone());
    let (server_owner, server_authority) = enrollment_authority_channel();
    let server_task = tokio::spawn(run_enrollment_reconciler(
        server.node.clone(),
        server.node_identity.clone(),
        server_owner,
        cancel.clone(),
    ));
    let pairing_task = tokio::spawn(run_pairing_reconciler(
        server.node.clone(),
        server.node_identity.clone(),
        server_authority,
        cancel.clone(),
    ));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (outcomes, _) = load_status_enrollment_approvals(
            &member.node,
            member.node_identity.as_ref(),
            &member_peer,
        )
        .await
        .unwrap();
        if matches!(
            outcomes.get(&server_peer),
            Some(EnrollmentAuthorityOutcome::Current(_))
        ) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "member did not observe the initial signed route"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (member_owner, _member_authority) = enrollment_authority_channel();
    let member_task = tokio::spawn(run_enrollment_reconciler(
        member.node.clone(),
        member.node_identity.clone(),
        member_owner,
        cancel.clone(),
    ));
    let deadline = Instant::now() + Duration::from_secs(110);
    loop {
        let projection = store.load_projection().await.unwrap();
        if let Some(active) = projection.active.iter().find(|entry| {
            entry.request.candidate_did == member.node_identity.did()
                && entry.revision.sequence == 2
        }) {
            assert!(modeled("valid"));
            assert_ne!(active.request.request_id, first.request_id);
            assert_eq!(active.request.candidate_peer, member_peer);
            let lease =
                chrono::DateTime::parse_from_rfc3339(&active.revision.authorization_expires_at)
                    .unwrap()
                    - chrono::DateTime::parse_from_rfc3339(&active.decision.decided_at).unwrap();
            assert_eq!(lease.num_seconds(), 120);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "automatic renewal did not create the next generation"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    cancel.cancel();
    server_task.await.unwrap().unwrap();
    member_task.await.unwrap().unwrap();
    pairing_task.await.unwrap().unwrap();
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
