use super::super::super::principal_identity::PrincipalIdentity;
use super::super::route_manager::ClientRouteManager;
use super::super::sync_state::ClientSyncStateOwner;
use super::authority::{
    apply_current_generational_conflicts, raw_authority_targets, scoped_authority_outcomes,
};
use super::projection::project_desktop_approval;
use super::reconcile::{
    authenticate_enrolled_server, demote_enrollment_peer, enrolled_server_address,
    prioritized_current_approvals, reconcile_status_enrollment_approvals, ApprovedStatusEnrollment,
    DesktopApprovalProjection, EnrollmentAuthorityOutcome,
};
use super::rows::{EnrollmentRevisionRow, EnrollmentRouteReceiptRow, STATUS_ENROLLMENT_QUERY};
use super::*;
use crate::client::peer_directory::RemovalCause;
use defra_p2p_adapter::P2pDocumentRequest;
use defra_p2p_adapter::TransportPeerId;
use gents::runtime::agent::p2p_reconcile::enrollment_candidate::enrollment_request_input;
use gents_protocol::enrollment::derive_enrollment_id;
use gents_protocol::enrollment::{
    AuthorizationRevisionKind, AuthorizationRevisionRecord, EnrollmentDecisionRecord,
    EnrollmentRouteReceiptDirection, EnrollmentRouteReceiptRecord, ENROLLMENT_PROTOCOL_VERSION,
};
use p2p::iroh::parse_public_peer_addr;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRequestCase {
    name: String,
    requested: gents::runtime::agent::p2p_reconcile::enrollment::EnrollmentRequestAuthority,
    current: gents::runtime::agent::p2p_reconcile::enrollment::EnrollmentRequestAuthority,
    has_decision: bool,
    current_approval: bool,
    route_applied: bool,
    expected_satisfied: bool,
    request_fresh: bool,
    expected_visible: bool,
}

fn pending_request_cases() -> Vec<PendingRequestCase> {
    let snapshot: Value = gents_lean_contract::load_contract_snapshot()
        .expect("Lean pending enrollment request cases");
    serde_json::from_value(snapshot["enrollment_pending_request_cases"].clone())
        .expect("decode pending enrollment request cases")
}

#[test]
fn generated_pending_request_satisfaction_matches_lean() {
    let cases = pending_request_cases();
    assert!(!cases.is_empty());
    for case in cases {
        assert_eq!(
            gents::runtime::agent::p2p_reconcile::enrollment::pending_request_satisfied(
                &case.requested,
                &case.current,
                case.has_decision,
                case.current_approval,
                case.route_applied,
            ),
            case.expected_satisfied,
            "{}",
            case.name,
        );
        assert_eq!(
            active_request_visible(
                case.request_fresh,
                case.has_decision,
                case.current_approval,
                case.route_applied
            ),
            case.expected_visible,
            "{}",
            case.name,
        );
    }
}

fn approval(server_peer: &str, owner_agent: &str) -> ApprovedStatusEnrollment {
    ApprovedStatusEnrollment {
        network_id: "network".into(),
        request_id: format!("request-{server_peer}-{owner_agent}"),
        server_peer: server_peer.into(),
        server_ticket: "ticket".into(),
        admin_did: "did:key:admin".into(),
        owner_node: owner_agent.into(),
        request_digest: format!("digest-{server_peer}-{owner_agent}"),
        authorization_sequence: 1,
        authorization_expires_at: "2026-09-29T00:00:00Z".into(),
        decided_at: "2026-08-29T00:00:00Z".into(),
    }
}

#[tokio::test]
async fn managed_enrollment_discovers_restarted_endpoint_without_changing_authority() {
    use crate::client::PeerDirectory;
    use wiremock::{matchers::path, Mock, MockServer, ResponseTemplate};

    let peer = "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
    let other = "352aec0771cb90685b41d6f7fd7d89b8586d38a3ed7fe1d08f98f5592b453365";
    let owner = "did:key:managed-owner";
    for (live_owner, live_peer, address_peer, accepted) in [
        (owner, peer, peer, true),
        ("did:key:another-owner", peer, peer, false),
        (owner, other, other, false),
        (owner, peer, other, false),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("runtime");
        std::fs::create_dir(&home).unwrap();
        let server = MockServer::start().await;
        let graphql = format!("{}/api/v0/graphql", server.uri());
        let new_address = format!("127.0.0.1:56001/p2p/{address_peer}");
        for (route, body) in [
            (
                "/status",
                json!({"node_did": live_owner, "lifecycle": "ready"}),
            ),
            (
                "/api/v0/p2p/shareable-address",
                json!({"address": new_address}),
            ),
            ("/api/v0/node/identity", json!({"peer_id": live_peer})),
        ] {
            Mock::given(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
        }
        std::fs::write(
            home.join("init.json"),
            json!({
                "node_name": "managed", "node_did": live_owner,
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            home.join(gents::store::local::home::RUNTIME_STATE_FILE_NAME),
            json!({
                "node_name": "managed", "node_did": live_owner,
                "graphql": graphql, "p2p_transport": "iroh", "p2p_peer_id": live_peer,
            })
            .to_string(),
        )
        .unwrap();

        let mut approval = approval(peer, owner);
        approval.server_ticket = format!("127.0.0.1:56000/p2p/{peer}");
        let unchanged = approval.clone();
        let mut directory = PeerDirectory::open_writer(temp.path().join("peers.json"))
            .await
            .unwrap();
        directory
            .upsert_local_standard_peer(
                "Managed",
                &approval.server_ticket,
                owner,
                &graphql,
                home.to_str().unwrap(),
            )
            .await
            .unwrap();
        let known = directory
            .upsert_enrollment_peer(
                peer,
                "Managed",
                &approval.server_ticket,
                owner,
                &approval.network_id,
                &approval.request_id,
                &approval.request_digest,
                &approval.admin_did,
                approval.authorization_sequence,
                &approval.authorization_expires_at,
            )
            .await
            .unwrap();
        let resolved = enrolled_server_address(&approval, Some(&known)).await;
        if accepted {
            let address = resolved.unwrap();
            assert_eq!(address, new_address);
            let updated = directory
                .upsert_enrollment_peer(
                    peer,
                    "Managed",
                    &address,
                    owner,
                    &approval.network_id,
                    &approval.request_id,
                    &approval.request_digest,
                    &approval.admin_did,
                    approval.authorization_sequence,
                    &approval.authorization_expires_at,
                )
                .await
                .unwrap();
            assert_eq!(updated.addr, new_address);
            assert_eq!(
                updated.enrollment_request_digest,
                known.enrollment_request_digest
            );
            assert_eq!(
                updated.enrollment_authorization_sequence,
                known.enrollment_authorization_sequence
            );
            assert_eq!(updated.enrollment_admin_did, known.enrollment_admin_did);
            assert!(!updated.pairing_ready);
        } else {
            assert!(resolved.is_err());
            assert_eq!(directory.records()[0], known);
        }
        assert_eq!(approval, unchanged);
        let mut remote = known.clone();
        remote.local_node_home = None;
        assert_eq!(
            enrolled_server_address(&approval, Some(&remote))
                .await
                .unwrap(),
            approval.server_ticket
        );
        assert_eq!(
            enrolled_server_address(&approval, None).await.unwrap(),
            approval.server_ticket
        );
    }
}

#[test]
fn enrollment_authentication_prioritizes_newest_unknown_peer() {
    let mut old_unknown = approval("peer-old", "agent-old");
    old_unknown.decided_at = "2026-08-29T00:00:01Z".into();
    let mut active = approval("peer-active", "agent-active");
    active.decided_at = "2026-08-29T00:00:03Z".into();
    let mut known = approval("peer-known", "agent-known");
    known.decided_at = "2026-08-29T00:00:04Z".into();
    let outcomes = BTreeMap::from([
        (
            old_unknown.server_peer.clone(),
            EnrollmentAuthorityOutcome::Current(old_unknown),
        ),
        (
            active.server_peer.clone(),
            EnrollmentAuthorityOutcome::Current(active),
        ),
        (
            known.server_peer.clone(),
            EnrollmentAuthorityOutcome::Current(known),
        ),
    ]);

    let ordered =
        prioritized_current_approvals(&outcomes, &BTreeSet::from(["peer-known".to_string()]))
            .into_iter()
            .map(|(peer_id, _)| peer_id)
            .collect::<Vec<_>>();

    assert_eq!(ordered, vec!["peer-active", "peer-old", "peer-known"]);
}

#[test]
fn current_authority_rejects_transport_and_owner_collisions() {
    let duplicate_peer = scoped_authority_outcomes(
        vec![approval("peer", "agent-a"), approval("peer", "agent-b")],
        BTreeMap::new(),
    );
    assert!(matches!(
        duplicate_peer.get("peer"),
        Some(EnrollmentAuthorityOutcome::Conflicted { .. })
    ));

    let duplicate_owner = scoped_authority_outcomes(
        vec![approval("peer-a", "agent"), approval("peer-b", "agent")],
        BTreeMap::new(),
    );
    assert!(duplicate_owner
        .values()
        .all(|outcome| matches!(outcome, EnrollmentAuthorityOutcome::Conflicted { .. })));

    let distinct = scoped_authority_outcomes(
        vec![approval("peer-a", "agent-a"), approval("peer-b", "agent-b")],
        BTreeMap::new(),
    );
    assert!(distinct
        .values()
        .all(|outcome| matches!(outcome, EnrollmentAuthorityOutcome::Current(_))));
}

#[test]
fn hostile_rows_are_attributed_to_only_the_owned_server_scope() {
    let requests = BTreeMap::from([
        ("request-a".into(), ("peer-a".into(), "network-a".into())),
        ("request-b".into(), ("peer-b".into(), "network-b".into())),
    ]);
    let networks = BTreeMap::from([
        ("network-a".into(), vec!["peer-a".into()]),
        ("network-b".into(), vec!["peer-b".into()]),
    ]);
    let malformed_a = serde_json::json!({
        "request_id": "request-a",
        "network_id": "network-a",
        "member_did": "did:key:local"
    });
    assert_eq!(
        raw_authority_targets(
            &malformed_a,
            &requests,
            &networks,
            "did:key:local",
            "local-peer",
        ),
        ["peer-a"]
    );
    let malformed_decision_without_request_id = serde_json::json!({
        "network_id": "network-a",
        "candidate_did": "did:key:local",
        "candidate_peer": "local-peer",
        "authorization_sequence": 8,
    });
    assert_eq!(
        raw_authority_targets(
            &malformed_decision_without_request_id,
            &requests,
            &networks,
            "did:key:local",
            "local-peer",
        ),
        ["peer-a"]
    );
    let unrelated = serde_json::json!({
        "network_id": "network-z",
        "member_did": "did:key:other"
    });
    assert!(raw_authority_targets(
        &unrelated,
        &requests,
        &networks,
        "did:key:local",
        "local-peer",
    )
    .is_empty());

    let outcomes = scoped_authority_outcomes(
        vec![approval("peer-a", "agent-a"), approval("peer-b", "agent-b")],
        BTreeMap::from([("peer-a".into(), vec!["malformed relevant row".into()])]),
    );
    assert!(matches!(
        outcomes.get("peer-a"),
        Some(EnrollmentAuthorityOutcome::Conflicted { .. })
    ));
    assert!(matches!(
        outcomes.get("peer-b"),
        Some(EnrollmentAuthorityOutcome::Current(_))
    ));
}

#[test]
fn malformed_authority_rows_fail_closed_before_projection() {
    let revision = EnrollmentRevisionRow {
        protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
        revision_id: "revision".into(),
        request_id: "request".into(),
        request_digest: "digest".into(),
        network_id: "network".into(),
        admin_did: "did:key:admin".into(),
        member_did: "did:key:member".into(),
        member_peer: "member-peer".into(),
        owner_agent: "did:key:agent".into(),
        sequence: -1,
        authorization_expires_at: "2099-09-29T00:00:00Z".into(),
        kind: "active".into(),
        issued_at: "2026-08-29T00:00:00Z".into(),
        signer_did: "did:key:admin".into(),
        admin_sig: bs58::encode([0_u8; 64]).into_string(),
    };
    assert!(revision.to_record().is_err());

    let receipt = EnrollmentRouteReceiptRow {
        protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
        receipt_id: "receipt".into(),
        request_id: "request".into(),
        request_digest: "digest".into(),
        network_id: "network".into(),
        admin_did: "did:key:admin".into(),
        member_did: "did:key:member".into(),
        member_peer: "member-peer".into(),
        server_peer: "server-peer".into(),
        owner_agent: "did:key:agent".into(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".into(),
        direction: "server_to_client".into(),
        applied_at: "2026-08-29T00:00:00Z".into(),
        signer_did: "did:key:admin".into(),
        admin_sig: bs58::encode([0_u8; 64]).into_string(),
    };
    assert!(receipt.to_record().is_err());
}

#[test]
fn malformed_historical_authority_recovers_only_after_a_higher_generation() {
    let mut conflicts = BTreeMap::new();
    apply_current_generational_conflicts(
        &[ApprovedStatusEnrollment {
            authorization_sequence: 8,
            ..approval("peer-a", "agent-a")
        }],
        BTreeMap::from([(
            "peer-a".into(),
            vec![(Some(7), "old malformed revision".into())],
        )]),
        &mut conflicts,
    );
    assert!(conflicts.is_empty());

    for hostile_generation in [None, Some(8), Some(9)] {
        let mut conflicts = BTreeMap::new();
        apply_current_generational_conflicts(
            &[ApprovedStatusEnrollment {
                authorization_sequence: 8,
                ..approval("peer-a", "agent-a")
            }],
            BTreeMap::from([(
                "peer-a".into(),
                vec![(hostile_generation, "current malformed revision".into())],
            )]),
            &mut conflicts,
        );
        assert!(conflicts.contains_key("peer-a"));
    }
}

#[tokio::test]
async fn signed_current_receipt_opens_generation_and_revocation_closes_it() {
    use crate::client::core::route_manager::{
        combined_route_readiness, enrollment_remote_route_state, RouteReconcileState,
    };
    use crate::client::paths::DesktopPaths;
    use gents_protocol::enrollment::{
        derive_decision_id, derive_enrollment_id, derive_revision_id, derive_route_receipt_id,
        encode_offer, EnrollmentOfferRecord,
    };

    let temp = tempfile::tempdir().unwrap();
    let admin =
        PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
            .await
            .unwrap();
    let candidate =
        PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("candidate")))
            .await
            .unwrap();
    let server_ticket =
        "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
    let server_peer = parse_public_peer_addr(server_ticket).unwrap().0.to_string();
    let issued = Utc::now();
    let issued_at = issued.to_rfc3339_opts(SecondsFormat::Secs, true);
    let request_expires_at =
        (issued + chrono::Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let authorization_expires_at =
        (issued + chrono::Duration::days(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut offer = EnrollmentOfferRecord {
        version: ENROLLMENT_PROTOCOL_VERSION,
        offer_id: "offer-1".into(),
        challenge: "challenge-1".into(),
        network_id: "network-1".into(),
        admin_did: admin.did().into(),
        server_peer: server_peer.clone(),
        server_ticket: server_ticket.into(),
        owner_agent: "did:key:owner".into(),
        profile: "client".into(),
        schema_fingerprint: enrollment_schema_fingerprint(),
        issued_at: issued_at.clone(),
        expires_at: request_expires_at.clone(),
        admin_sig: Vec::new(),
    };
    offer.admin_sig = admin.sign(&offer.signing_payload()).unwrap();
    let offer_token = encode_offer(&offer).unwrap();

    let client_nonce = "nonce-1";
    let request_id = format!(
        "enroll-{}",
        derive_enrollment_id(
            "gents-enrollment-request-id-v1",
            &[
                &offer.offer_id,
                candidate.did(),
                "client-peer",
                client_nonce,
            ],
        )
    );
    let mut request = EnrollmentRequestRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        request_id,
        request_digest: String::new(),
        offer_id: offer.offer_id.clone(),
        offer_token: offer_token.clone(),
        challenge: offer.challenge.clone(),
        network_id: offer.network_id.clone(),
        admin_did: offer.admin_did.clone(),
        server_peer: server_peer.clone(),
        candidate_did: candidate.did().into(),
        candidate_peer: "client-peer".into(),
        candidate_ticket: "client-ticket".into(),
        owner_agent: offer.owner_agent.clone(),
        profile: offer.profile.clone(),
        client_nonce: client_nonce.into(),
        issued_at: issued_at.clone(),
        expires_at: request_expires_at,
        candidate_sig: Vec::new(),
    };
    request.request_digest = request.computed_digest();
    request.candidate_sig = candidate.sign(&request.signing_payload()).unwrap();

    let mut decision = EnrollmentDecisionRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        decision_id: derive_decision_id(&request.request_id, &request.request_digest),
        request_id: request.request_id.clone(),
        request_digest: request.request_digest.clone(),
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        candidate_did: request.candidate_did.clone(),
        candidate_peer: request.candidate_peer.clone(),
        owner_agent: request.owner_agent.clone(),
        decision: EnrollmentDecisionKind::Approved,
        authorization_sequence: 1,
        authorization_expires_at: authorization_expires_at.clone(),
        decided_at: issued_at.clone(),
        signer_did: admin.did().into(),
        admin_sig: Vec::new(),
    };
    decision.admin_sig = admin.sign(&decision.signing_payload()).unwrap();
    let mut revision = AuthorizationRevisionRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        revision_id: derive_revision_id(
            &request.network_id,
            &request.candidate_did,
            1,
            &AuthorizationRevisionKind::Active,
            &request.request_digest,
        ),
        request_id: request.request_id.clone(),
        request_digest: request.request_digest.clone(),
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        member_did: request.candidate_did.clone(),
        member_peer: request.candidate_peer.clone(),
        owner_agent: request.owner_agent.clone(),
        sequence: 1,
        authorization_expires_at: authorization_expires_at.clone(),
        kind: AuthorizationRevisionKind::Active,
        issued_at: issued_at.clone(),
        signer_did: admin.did().into(),
        admin_sig: Vec::new(),
    };
    revision.admin_sig = admin.sign(&revision.signing_payload()).unwrap();
    let direction = EnrollmentRouteReceiptDirection::ClientToServer;
    let mut receipt = EnrollmentRouteReceiptRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        receipt_id: derive_route_receipt_id(
            &request.request_id,
            &request.request_digest,
            1,
            &direction,
        ),
        request_id: request.request_id.clone(),
        request_digest: request.request_digest.clone(),
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        member_did: request.candidate_did.clone(),
        member_peer: request.candidate_peer.clone(),
        server_peer: request.server_peer.clone(),
        owner_agent: request.owner_agent.clone(),
        authorization_sequence: 1,
        authorization_expires_at: authorization_expires_at.clone(),
        direction,
        applied_at: issued_at.clone(),
        signer_did: admin.did().into(),
        admin_sig: Vec::new(),
    };
    receipt.admin_sig = admin.sign(&receipt.signing_payload()).unwrap();

    let request_row = EnrollmentRequestRow {
        doc_id: "request-doc".into(),
        protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
        request_id: request.request_id.clone(),
        request_digest: request.request_digest.clone(),
        offer_id: request.offer_id.clone(),
        offer_token,
        challenge: request.challenge.clone(),
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        server_peer: request.server_peer.clone(),
        candidate_did: request.candidate_did.clone(),
        candidate_peer: request.candidate_peer.clone(),
        candidate_ticket: request.candidate_ticket.clone(),
        owner_agent: request.owner_agent.clone(),
        profile: request.profile.clone(),
        client_nonce: request.client_nonce.clone(),
        issued_at: request.issued_at.clone(),
        expires_at: request.expires_at.clone(),
        candidate_sig: bs58::encode(&request.candidate_sig).into_string(),
    };
    let decision_row = EnrollmentDecisionRow {
        protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
        decision_id: decision.decision_id.clone(),
        request_id: decision.request_id.clone(),
        request_digest: decision.request_digest.clone(),
        network_id: decision.network_id.clone(),
        admin_did: decision.admin_did.clone(),
        candidate_did: decision.candidate_did.clone(),
        candidate_peer: decision.candidate_peer.clone(),
        owner_agent: decision.owner_agent.clone(),
        decision: "approved".into(),
        authorization_sequence: 1,
        authorization_expires_at: authorization_expires_at.clone(),
        decided_at: decision.decided_at.clone(),
        signer_did: decision.signer_did.clone(),
        admin_sig: bs58::encode(&decision.admin_sig).into_string(),
    };
    let revision_row = |record: &AuthorizationRevisionRecord| EnrollmentRevisionRow {
        protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
        revision_id: record.revision_id.clone(),
        request_id: record.request_id.clone(),
        request_digest: record.request_digest.clone(),
        network_id: record.network_id.clone(),
        admin_did: record.admin_did.clone(),
        member_did: record.member_did.clone(),
        member_peer: record.member_peer.clone(),
        owner_agent: record.owner_agent.clone(),
        sequence: record.sequence as i64,
        authorization_expires_at: record.authorization_expires_at.clone(),
        kind: record.kind.as_str().into(),
        issued_at: record.issued_at.clone(),
        signer_did: record.signer_did.clone(),
        admin_sig: bs58::encode(&record.admin_sig).into_string(),
    };
    let receipt_row = EnrollmentRouteReceiptRow {
        protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
        receipt_id: receipt.receipt_id.clone(),
        request_id: receipt.request_id.clone(),
        request_digest: receipt.request_digest.clone(),
        network_id: receipt.network_id.clone(),
        admin_did: receipt.admin_did.clone(),
        member_did: receipt.member_did.clone(),
        member_peer: receipt.member_peer.clone(),
        server_peer: receipt.server_peer.clone(),
        owner_agent: receipt.owner_agent.clone(),
        authorization_sequence: 1,
        authorization_expires_at: authorization_expires_at.clone(),
        direction: "client_to_server".into(),
        applied_at: receipt.applied_at.clone(),
        signer_did: receipt.signer_did.clone(),
        admin_sig: bs58::encode(&receipt.admin_sig).into_string(),
    };
    let pins = BTreeMap::from([("network-1".into(), vec![admin.did().into()])]);

    assert!(matches!(
        project_desktop_approval(
            &request_row,
            std::slice::from_ref(&request_row),
            std::slice::from_ref(&decision_row),
            std::slice::from_ref(&revision_row(&revision)),
            &[],
            &pins,
            &candidate,
            "client-peer",
        )
        .await
        .unwrap(),
        DesktopApprovalProjection::CurrentWithoutRoute,
    ));
    let approved = match project_desktop_approval(
        &request_row,
        std::slice::from_ref(&request_row),
        std::slice::from_ref(&decision_row),
        std::slice::from_ref(&revision_row(&revision)),
        std::slice::from_ref(&receipt_row),
        &pins,
        &candidate,
        "client-peer",
    )
    .await
    .unwrap()
    {
        DesktopApprovalProjection::Routed(approved) => approved,
        _ => panic!("signed current receipt opens exact generation"),
    };
    assert_eq!(approved.request_digest, request.request_digest);
    assert_eq!(approved.authorization_sequence, 1);
    assert_eq!(approved.authorization_expires_at, authorization_expires_at);

    let (_directory, sync_state) = ClientSyncStateOwner::for_test(Vec::new(), Vec::new()).await;
    let configured = sync_state
        .upsert_enrollment_peer(
            &approved.server_peer,
            "Enrolled Agent",
            &approved.server_ticket,
            &approved.owner_node,
            &approved.network_id,
            &approved.request_id,
            &approved.request_digest,
            &approved.admin_did,
            approved.authorization_sequence,
            &approved.authorization_expires_at,
        )
        .await
        .unwrap();
    assert!(!configured.is_chat_ready_at(Utc::now()));
    let ready = combined_route_readiness(
        RouteReconcileState::Ready,
        enrollment_remote_route_state(true),
    )
    .expect("exact local applied evidence plus current receipt is decisive");
    let ready_record = sync_state
        .set_pairing_ready(&configured, ready)
        .await
        .unwrap()
        .expect("approved generation remains configured");
    assert!(
        ready_record.is_chat_ready_at(Utc::now()),
        "send gate opens only after both legs"
    );
    let stable = sync_state
        .upsert_enrollment_peer(
            &approved.server_peer,
            "Enrolled Agent",
            &approved.server_ticket,
            &approved.owner_node,
            &approved.network_id,
            &approved.request_id,
            &approved.request_digest,
            &approved.admin_did,
            approved.authorization_sequence,
            &approved.authorization_expires_at,
        )
        .await
        .unwrap();
    assert!(
        stable.pairing_ready,
        "exact idempotent generation stays ready"
    );

    let mut revoked = revision;
    revoked.sequence = 2;
    revoked.kind = AuthorizationRevisionKind::Revoked;
    revoked.revision_id = derive_revision_id(
        &request.network_id,
        &request.candidate_did,
        2,
        &revoked.kind,
        &request.request_digest,
    );
    revoked.admin_sig = admin.sign(&revoked.signing_payload()).unwrap();
    assert!(matches!(
        project_desktop_approval(
            &request_row,
            std::slice::from_ref(&request_row),
            std::slice::from_ref(&decision_row),
            &[revision_row(&revoked)],
            std::slice::from_ref(&receipt_row),
            &pins,
            &candidate,
            "client-peer",
        )
        .await
        .unwrap(),
        DesktopApprovalProjection::Absent,
    ));
    demote_enrollment_peer(&sync_state, &approved.server_peer).await;
    let revoked_record = sync_state.records().into_iter().next().unwrap();
    assert!(
        !revoked_record.is_chat_ready_at(Utc::now()),
        "revocation closes the send gate"
    );
    let replacement = sync_state
        .upsert_enrollment_peer(
            &approved.server_peer,
            "Enrolled Agent",
            &approved.server_ticket,
            &approved.owner_node,
            &approved.network_id,
            "replacement-request-id",
            "replacement-request-digest",
            &approved.admin_did,
            3,
            "2099-10-29T00:00:00Z",
        )
        .await
        .unwrap();
    assert!(
        !replacement.is_chat_ready_at(Utc::now()),
        "a new authorization generation must prove both route legs again"
    );
}

use defra_p2p_adapter::{
    ExplicitReplayCapabilityInput, P2PResult, P2pDocumentInfo, ReplicationFilters, ReplicatorInfo,
};
struct EnrollmentTransport {
    peer: String,
    resolved: Option<identity::Did>,
    connected: std::sync::atomic::AtomicBool,
    dials: std::sync::atomic::AtomicUsize,
    observations: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl P2POps for EnrollmentTransport {
    async fn local_peer_id(&self) -> P2PResult<String> {
        unimplemented!()
    }
    async fn listen_addresses(&self) -> P2PResult<Vec<String>> {
        unimplemented!()
    }
    async fn connected_peers(&self) -> P2PResult<Vec<String>> {
        Ok(
            if self.connected.load(std::sync::atomic::Ordering::SeqCst) {
                vec![self.peer.clone()]
            } else {
                vec![]
            },
        )
    }
    async fn connect_peer(&self, _addr: &str) -> P2PResult<()> {
        self.dials.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.connected
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    async fn disconnect_peer(&self, _addr: &str) -> P2PResult<()> {
        self.connected
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    async fn get_replicators(&self) -> P2PResult<Vec<ReplicatorInfo>> {
        unimplemented!()
    }
    async fn add_replicator(
        &self,
        _collections: Vec<String>,
        _addr: Option<&str>,
        _filters: ReplicationFilters,
        _explicit_replay_capabilities: Vec<ExplicitReplayCapabilityInput>,
        _expected_authorizer_did: Option<&str>,
    ) -> P2PResult<()> {
        unimplemented!()
    }
    async fn remove_replicator(
        &self,
        _collections: Vec<String>,
        _addr: Option<&str>,
    ) -> P2PResult<()> {
        Ok(())
    }
    async fn get_collections(&self) -> P2PResult<Vec<String>> {
        unimplemented!()
    }
    async fn add_collections(&self, _collections: Vec<String>) -> P2PResult<()> {
        unimplemented!()
    }
    async fn remove_collections(&self, _collections: Vec<String>) -> P2PResult<()> {
        unimplemented!()
    }
    async fn get_documents(&self) -> P2PResult<Vec<P2pDocumentInfo>> {
        unimplemented!()
    }
    async fn add_documents(&self, _docs: Vec<P2pDocumentRequest>) -> P2PResult<()> {
        unimplemented!()
    }
    async fn remove_documents(&self, _docs: Vec<P2pDocumentRequest>) -> P2PResult<()> {
        unimplemented!()
    }
    async fn sync_documents(
        &self,
        _collection_name: &str,
        _doc_ids: Vec<String>,
        _timeout: Option<std::time::Duration>,
    ) -> P2PResult<()> {
        unimplemented!()
    }
    async fn sync_branchable_collection(&self, _collection_id: &str) -> P2PResult<()> {
        unimplemented!()
    }
    async fn sync_collection_versions(&self, _version_ids: Vec<String>) -> P2PResult<()> {
        unimplemented!()
    }
    async fn resolve_peer_identity(&self, _: &TransportPeerId) -> P2PResult<Option<identity::Did>> {
        self.observations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match &self.resolved {
            Some(did) => Ok(Some(did.clone())),
            None => Err(P2PError::unsupported("identity challenge rejected")),
        }
    }
}
#[tokio::test]
async fn repeated_enrollment_observations_reuse_connection_but_authenticate_fail_closed() {
    let transport = Arc::new(EnrollmentTransport {
        peer: "approved-peer".into(),
        resolved: None,
        connected: false.into(),
        dials: 0.into(),
        observations: 0.into(),
    });
    let p2p: Arc<dyn P2POps> = transport.clone();
    let approval = ApprovedStatusEnrollment {
        network_id: String::new(),
        request_id: String::new(),
        server_peer: transport.peer.clone(),
        server_ticket: "ticket".into(),
        admin_did: "did:key:approved".into(),
        owner_node: String::new(),
        request_digest: String::new(),
        authorization_sequence: 1,
        authorization_expires_at: String::new(),
        decided_at: String::new(),
    };
    for _ in 0..8 {
        assert!(authenticate_enrolled_server(&p2p, &approval, None)
            .await
            .is_err());
    }
    use std::sync::atomic::Ordering::SeqCst;
    assert_eq!(transport.dials.load(SeqCst), 1);
    assert_eq!(transport.observations.load(SeqCst), 8);
    transport.connected.store(false, SeqCst);
    assert!(authenticate_enrolled_server(&p2p, &approval, None)
        .await
        .is_err());
    assert_eq!(transport.dials.load(SeqCst), 2);
    assert_eq!(transport.observations.load(SeqCst), 9);
}

struct SignedEnrollmentAuthority {
    offer: gents_protocol::enrollment::EnrollmentOfferRecord,
    offer_token: String,
    request: EnrollmentRequestRecord,
    decision: EnrollmentDecisionRecord,
    revision: AuthorizationRevisionRecord,
    revoked: AuthorizationRevisionRecord,
    receipt: EnrollmentRouteReceiptRecord,
}

fn signed_enrollment_authority(
    admin: &PrincipalIdentity,
    candidate: &PrincipalIdentity,
    candidate_peer: &str,
) -> SignedEnrollmentAuthority {
    signed_enrollment_authority_at(admin, candidate, candidate_peer, Utc::now(), "1")
}

fn signed_enrollment_authority_at(
    admin: &PrincipalIdentity,
    candidate: &PrincipalIdentity,
    candidate_peer: &str,
    issued: DateTime<Utc>,
    generation: &str,
) -> SignedEnrollmentAuthority {
    use gents_protocol::enrollment::{
        derive_decision_id, derive_revision_id, derive_route_receipt_id, encode_offer,
        EnrollmentOfferRecord,
    };

    let server_ticket =
        "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
    let server_peer = parse_public_peer_addr(server_ticket).unwrap().0.to_string();
    let issued_at = issued.to_rfc3339_opts(SecondsFormat::Secs, true);
    let request_expires_at =
        (issued + chrono::Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let authorization_expires_at =
        (issued + chrono::Duration::days(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut offer = EnrollmentOfferRecord {
        version: ENROLLMENT_PROTOCOL_VERSION,
        offer_id: format!("offer-{generation}"),
        challenge: format!("challenge-{generation}"),
        network_id: "network-1".into(),
        admin_did: admin.did().into(),
        server_peer: server_peer.clone(),
        server_ticket: server_ticket.into(),
        owner_agent: "did:key:owner".into(),
        profile: "client".into(),
        schema_fingerprint: enrollment_schema_fingerprint(),
        issued_at: issued_at.clone(),
        expires_at: request_expires_at.clone(),
        admin_sig: Vec::new(),
    };
    offer.admin_sig = admin.sign(&offer.signing_payload()).unwrap();
    let offer_token = encode_offer(&offer).unwrap();

    let client_nonce = format!("nonce-{generation}");
    let request_id = format!(
        "enroll-{}",
        derive_enrollment_id(
            "gents-enrollment-request-id-v1",
            &[
                &offer.offer_id,
                candidate.did(),
                candidate_peer,
                &client_nonce
            ],
        )
    );
    let mut request = EnrollmentRequestRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        request_id,
        request_digest: String::new(),
        offer_id: offer.offer_id.clone(),
        offer_token: offer_token.clone(),
        challenge: offer.challenge.clone(),
        network_id: offer.network_id.clone(),
        admin_did: offer.admin_did.clone(),
        server_peer: offer.server_peer.clone(),
        candidate_did: candidate.did().into(),
        candidate_peer: candidate_peer.into(),
        candidate_ticket: "client-ticket".into(),
        owner_agent: offer.owner_agent.clone(),
        profile: offer.profile.clone(),
        client_nonce,
        issued_at: issued_at.clone(),
        expires_at: request_expires_at,
        candidate_sig: Vec::new(),
    };
    request.request_digest = request.computed_digest();
    request.candidate_sig = candidate.sign(&request.signing_payload()).unwrap();

    let mut decision = EnrollmentDecisionRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        decision_id: derive_decision_id(&request.request_id, &request.request_digest),
        request_id: request.request_id.clone(),
        request_digest: request.request_digest.clone(),
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        candidate_did: request.candidate_did.clone(),
        candidate_peer: request.candidate_peer.clone(),
        owner_agent: request.owner_agent.clone(),
        decision: EnrollmentDecisionKind::Approved,
        authorization_sequence: 1,
        authorization_expires_at: authorization_expires_at.clone(),
        decided_at: issued_at.clone(),
        signer_did: admin.did().into(),
        admin_sig: Vec::new(),
    };
    decision.admin_sig = admin.sign(&decision.signing_payload()).unwrap();

    let signed_revision =
        |sequence: u64, kind: AuthorizationRevisionKind| -> AuthorizationRevisionRecord {
            let mut revision = AuthorizationRevisionRecord {
                protocol_version: ENROLLMENT_PROTOCOL_VERSION,
                revision_id: derive_revision_id(
                    &request.network_id,
                    &request.candidate_did,
                    sequence,
                    &kind,
                    &request.request_digest,
                ),
                request_id: request.request_id.clone(),
                request_digest: request.request_digest.clone(),
                network_id: request.network_id.clone(),
                admin_did: request.admin_did.clone(),
                member_did: request.candidate_did.clone(),
                member_peer: request.candidate_peer.clone(),
                owner_agent: request.owner_agent.clone(),
                sequence,
                authorization_expires_at: authorization_expires_at.clone(),
                kind,
                issued_at: issued_at.clone(),
                signer_did: admin.did().into(),
                admin_sig: Vec::new(),
            };
            revision.admin_sig = admin.sign(&revision.signing_payload()).unwrap();
            revision
        };
    let revision = signed_revision(1, AuthorizationRevisionKind::Active);
    let revoked = signed_revision(2, AuthorizationRevisionKind::Revoked);

    let direction = EnrollmentRouteReceiptDirection::ClientToServer;
    let mut receipt = EnrollmentRouteReceiptRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        receipt_id: derive_route_receipt_id(
            &request.request_id,
            &request.request_digest,
            1,
            &direction,
        ),
        request_id: request.request_id.clone(),
        request_digest: request.request_digest.clone(),
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        member_did: request.candidate_did.clone(),
        member_peer: request.candidate_peer.clone(),
        server_peer: request.server_peer.clone(),
        owner_agent: request.owner_agent.clone(),
        authorization_sequence: 1,
        authorization_expires_at: authorization_expires_at.clone(),
        direction,
        applied_at: issued_at.clone(),
        signer_did: admin.did().into(),
        admin_sig: Vec::new(),
    };
    receipt.admin_sig = admin.sign(&receipt.signing_payload()).unwrap();

    SignedEnrollmentAuthority {
        offer,
        offer_token,
        request,
        decision,
        revision,
        revoked,
        receipt,
    }
}

async fn commit_document(core: &ClientCore, stage: &'static str, mutation: &str) {
    gents::store::config_client::ConfigAccess::write_local(&core.node_arc(), stage, mutation)
        .await
        .expect("commit enrollment authority document");
}

async fn commit_admin_pin(core: &ClientCore, authority: &SignedEnrollmentAuthority) {
    let pin_key = escape_graphql_string(&format!(
        "pin-{}",
        derive_enrollment_id(
            "gents-network-admin-pin-v1",
            &[&authority.request.network_id],
        )
    ));
    let mutation = format!(
        r#"mutation {{ create_NetworkAdminPin(input: {{
                pin_key: "{pin_key}",
                network_id: "{}",
                admin_did: "{}",
                offer_id: "{}",
                confirmed_at: "{}"
            }}) {{ _docID }} }}"#,
        escape_graphql_string(&authority.request.network_id),
        escape_graphql_string(&authority.request.admin_did),
        escape_graphql_string(&authority.request.offer_id),
        escape_graphql_string(&authority.request.issued_at),
    );
    commit_document(core, "desktop.enrollment.test.admin_pin", &mutation).await;
}

async fn commit_enrollment_request(core: &ClientCore, request: &EnrollmentRequestRecord) {
    let input = enrollment_request_input(request);
    let mutation =
        format!("mutation {{ create_NetworkEnrollmentRequest(input: {input}) {{ _docID }} }}");
    commit_document(core, "desktop.enrollment.test.request", &mutation).await;
}

async fn commit_decision(core: &ClientCore, authority: &SignedEnrollmentAuthority) {
    let decision = &authority.decision;
    let mutation = format!(
        r#"mutation {{ create_NetworkEnrollmentDecision(input: {{
                protocol_version: {}, decision_id: "{}", request_id: "{}", request_digest: "{}",
                network_id: "{}", admin_did: "{}", candidate_did: "{}", candidate_peer: "{}",
                owner_agent: "{}", decision: "{}", authorization_sequence: {},
                authorization_expires_at: "{}", decided_at: "{}", signer_did: "{}", admin_sig: "{}"
            }}) {{ _docID }} }}"#,
        decision.protocol_version,
        escape_graphql_string(&decision.decision_id),
        escape_graphql_string(&decision.request_id),
        escape_graphql_string(&decision.request_digest),
        escape_graphql_string(&decision.network_id),
        escape_graphql_string(&decision.admin_did),
        escape_graphql_string(&decision.candidate_did),
        escape_graphql_string(&decision.candidate_peer),
        escape_graphql_string(&decision.owner_agent),
        decision.decision.as_str(),
        decision.authorization_sequence,
        escape_graphql_string(&decision.authorization_expires_at),
        escape_graphql_string(&decision.decided_at),
        escape_graphql_string(&decision.signer_did),
        bs58::encode(&decision.admin_sig).into_string(),
    );
    commit_document(core, "desktop.enrollment.test.decision", &mutation).await;
}

async fn commit_revision(core: &ClientCore, revision: &AuthorizationRevisionRecord) {
    commit_revision_with_admin_sig(
        core,
        revision,
        &bs58::encode(&revision.admin_sig).into_string(),
    )
    .await;
}

async fn commit_revision_with_admin_sig(
    core: &ClientCore,
    revision: &AuthorizationRevisionRecord,
    admin_sig: &str,
) {
    let mutation = format!(
        r#"mutation {{ create_NetworkAuthorizationRevision(input: {{
                protocol_version: {}, revision_id: "{}", request_id: "{}", request_digest: "{}",
                network_id: "{}", admin_did: "{}", member_did: "{}", member_peer: "{}",
                owner_agent: "{}", sequence: {}, authorization_expires_at: "{}", kind: "{}",
                issued_at: "{}", signer_did: "{}", admin_sig: "{}"
            }}) {{ _docID }} }}"#,
        revision.protocol_version,
        escape_graphql_string(&revision.revision_id),
        escape_graphql_string(&revision.request_id),
        escape_graphql_string(&revision.request_digest),
        escape_graphql_string(&revision.network_id),
        escape_graphql_string(&revision.admin_did),
        escape_graphql_string(&revision.member_did),
        escape_graphql_string(&revision.member_peer),
        escape_graphql_string(&revision.owner_agent),
        revision.sequence,
        escape_graphql_string(&revision.authorization_expires_at),
        revision.kind.as_str(),
        escape_graphql_string(&revision.issued_at),
        escape_graphql_string(&revision.signer_did),
        escape_graphql_string(admin_sig),
    );
    commit_document(core, "desktop.enrollment.test.revision", &mutation).await;
}

async fn commit_route_receipt(core: &ClientCore, authority: &SignedEnrollmentAuthority) {
    let receipt = &authority.receipt;
    let mutation = format!(
        r#"mutation {{ create_NetworkEnrollmentRouteReceipt(input: {{
                protocol_version: {}, receipt_id: "{}", request_id: "{}", request_digest: "{}",
                network_id: "{}", admin_did: "{}", member_did: "{}", member_peer: "{}",
                server_peer: "{}", owner_agent: "{}", authorization_sequence: {},
                authorization_expires_at: "{}", direction: "{}", applied_at: "{}",
                signer_did: "{}", admin_sig: "{}"
            }}) {{ _docID }} }}"#,
        receipt.protocol_version,
        escape_graphql_string(&receipt.receipt_id),
        escape_graphql_string(&receipt.request_id),
        escape_graphql_string(&receipt.request_digest),
        escape_graphql_string(&receipt.network_id),
        escape_graphql_string(&receipt.admin_did),
        escape_graphql_string(&receipt.member_did),
        escape_graphql_string(&receipt.member_peer),
        escape_graphql_string(&receipt.server_peer),
        escape_graphql_string(&receipt.owner_agent),
        receipt.authorization_sequence,
        escape_graphql_string(&receipt.authorization_expires_at),
        receipt.direction.as_str(),
        escape_graphql_string(&receipt.applied_at),
        escape_graphql_string(&receipt.signer_did),
        bs58::encode(&receipt.admin_sig).into_string(),
    );
    commit_document(core, "desktop.enrollment.test.route_receipt", &mutation).await;
}

async fn start_unsupervised_core(root: std::path::PathBuf) -> ClientCore {
    let core = ClientCore::start_with_paths_and_options(
        crate::client::paths::DesktopPaths::from_root(root),
        super::super::ClientCoreOptions::local_only(),
    )
    .await
    .unwrap();
    if let Some(supervisor) = core.p2p_supervisor.lock().await.take() {
        supervisor.abort();
        let _ = supervisor.await;
    }
    core
}

async fn locally_retire(
    core: &ClientCore,
    authority: &SignedEnrollmentAuthority,
) -> crate::client::peer_directory::PeerRecord {
    core.sync_state
        .upsert_enrollment_peer(
            &authority.request.server_peer,
            "Enrolled server",
            &authority.offer.server_ticket,
            &authority.request.owner_agent,
            &authority.request.network_id,
            &authority.request.request_id,
            &authority.request.request_digest,
            &authority.request.admin_did,
            authority.decision.authorization_sequence,
            &authority.decision.authorization_expires_at,
        )
        .await
        .unwrap();
    let installed = core.peer_records().await;
    let [record] = installed.as_slice() else {
        panic!("one enrollment record is configured, got {installed:?}");
    };
    core.sync_state
        .queue_removal(record, RemovalCause::Operator)
        .await
        .unwrap()
        .expect("removal is queued");
    record.clone()
}

async fn reconcile_with_test_transport(core: &ClientCore, p2p: Arc<dyn P2POps>) {
    let principal = Arc::new(core.node_identity().clone());
    let route_manager = Arc::new(ClientRouteManager::new(
        core.node_arc(),
        Arc::clone(&p2p),
        Arc::clone(&principal),
        core.options().operator_identity_interaction,
    ));
    reconcile_status_enrollment_approvals(
        &core.node_arc(),
        &p2p,
        &principal,
        core.local_peer_id(),
        &core.sync_state,
        &route_manager,
    )
    .await
    .unwrap();
}

async fn commit_test_authority(core: &ClientCore, authority: &SignedEnrollmentAuthority) {
    commit_admin_pin(core, authority).await;
    commit_enrollment_request(core, &authority.request).await;
    commit_decision(core, authority).await;
    commit_revision(core, &authority.revision).await;
    commit_route_receipt(core, authority).await;
}

fn test_transport(authority: &SignedEnrollmentAuthority, authenticated: bool) -> Arc<dyn P2POps> {
    Arc::new(EnrollmentTransport {
        peer: authority.offer.server_ticket.clone(),
        resolved: authenticated
            .then(|| identity::Did::new(authority.request.admin_did.clone()).unwrap()),
        connected: false.into(),
        dials: 0.into(),
        observations: 0.into(),
    })
}

#[tokio::test]
async fn activation_authentication_error_is_observed_before_peer_exists_and_clears_on_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let core = start_unsupervised_core(temp.path().join("desktop")).await;
    let admin = PrincipalIdentity::load_or_create(&crate::client::paths::DesktopPaths::from_root(
        temp.path().join("admin"),
    ))
    .await
    .unwrap();
    let authority = signed_enrollment_authority_at(
        &admin,
        core.node_identity(),
        core.local_peer_id(),
        Utc::now() - chrono::Duration::minutes(10),
        "expired-request",
    );
    assert!(
        pending_request_cases()
            .into_iter()
            .find(|case| case.name == "visibility_false_true_true_false")
            .unwrap()
            .expected_visible
    );
    commit_test_authority(&core, &authority).await;
    let mut updates = core.sync_state.subscribe();
    reconcile_with_test_transport(&core, test_transport(&authority, false)).await;
    assert!(updates.has_changed().unwrap());
    assert!(updates.borrow_and_update().directory.is_empty());
    let requests = core.active_status_enrollment_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].state, "approved");
    assert_eq!(
        requests[0].activation_error.as_deref(),
        Some("authentication")
    );

    reconcile_with_test_transport(&core, test_transport(&authority, true)).await;
    assert!(core
        .sync_state
        .snapshot()
        .enrollment_activation_errors
        .is_empty());
    let requests = core.active_status_enrollment_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].activation_error, None);

    let record = core.peer_records().await.remove(0);
    let mut status = core.sync_state.peer(&record.peer_id).unwrap();
    status.last_error = Some("client route reconcile failed: test route failure".into());
    assert!(core.sync_state.replace_peer(&record, status.clone()));
    let requests = core.active_status_enrollment_requests().await.unwrap();
    assert_eq!(requests[0].activation_error, None);
    status.routes.push(super::super::ClientRouteStatus {
        route_id: "route".into(),
        direction: "outbound".into(),
        directory_id: record.peer_id.clone(),
        transport_peer_id: None,
        address: None,
        template: None,
        desired: true,
        applied: false,
        live_match: false,
        filter_summary: String::new(),
        last_error: Some("route failure".into()),
        retry_count: 1,
        last_retry_at: None,
        last_retry_error_class: None,
    });
    assert!(core.sync_state.replace_peer(&record, status.clone()));
    assert_eq!(
        core.active_status_enrollment_requests().await.unwrap()[0]
            .activation_error
            .as_deref(),
        Some("route")
    );
    status.routes.clear();
    status.last_error = None;
    assert!(core.sync_state.replace_peer(&record, status));
    assert_eq!(
        core.active_status_enrollment_requests().await.unwrap()[0].activation_error,
        None
    );
    core.sync_state
        .set_pairing_ready(&record, true)
        .await
        .unwrap();
    assert!(core
        .active_status_enrollment_requests()
        .await
        .unwrap()
        .is_empty());
    Arc::new(core).shutdown().await.unwrap();
}

#[tokio::test]
async fn expired_pending_and_expired_authorization_requests_are_not_visible() {
    let temp = tempfile::tempdir().unwrap();
    let core = start_unsupervised_core(temp.path().join("desktop")).await;
    let admin = PrincipalIdentity::load_or_create(&crate::client::paths::DesktopPaths::from_root(
        temp.path().join("admin"),
    ))
    .await
    .unwrap();
    let pending = signed_enrollment_authority_at(
        &admin,
        core.node_identity(),
        core.local_peer_id(),
        Utc::now() - chrono::Duration::minutes(10),
        "pending-expired",
    );
    let expired = signed_enrollment_authority_at(
        &admin,
        core.node_identity(),
        core.local_peer_id(),
        Utc::now() - chrono::Duration::days(2),
        "authorization-expired",
    );
    commit_test_authority(&core, &expired).await;
    commit_enrollment_request(&core, &pending.request).await;
    for name in [
        "visibility_false_false_false_false",
        "visibility_false_true_false_false",
    ] {
        assert!(
            !pending_request_cases()
                .into_iter()
                .find(|case| case.name == name)
                .unwrap()
                .expected_visible
        );
    }
    assert!(core
        .active_status_enrollment_requests()
        .await
        .unwrap()
        .is_empty());
    Arc::new(core).shutdown().await.unwrap();
}

#[tokio::test]
async fn expired_malformed_decision_does_not_hide_pending_request_but_fresh_failure_surfaces() {
    for expired in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let core = start_unsupervised_core(temp.path().join("desktop")).await;
        let admin = PrincipalIdentity::load_or_create(
            &crate::client::paths::DesktopPaths::from_root(temp.path().join("admin")),
        )
        .await
        .unwrap();
        let pending =
            signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
        let mut malformed = signed_enrollment_authority_at(
            &admin,
            core.node_identity(),
            core.local_peer_id(),
            if expired {
                Utc::now() - chrono::Duration::minutes(20)
            } else {
                Utc::now()
            },
            "malformed-decision",
        );
        malformed.decision.admin_sig.clear();
        commit_admin_pin(&core, &pending).await;
        commit_enrollment_request(&core, &pending.request).await;
        commit_enrollment_request(&core, &malformed.request).await;
        commit_decision(&core, &malformed).await;

        let listed = core.active_status_enrollment_requests().await;
        if expired {
            assert!(
                !pending_request_cases()
                    .into_iter()
                    .find(|case| case.name == "visibility_false_true_false_false")
                    .unwrap()
                    .expected_visible
            );
            let listed = listed.unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].request_id, pending.request.request_id);
            assert_eq!(listed[0].state, "pending_approval");
        } else {
            assert!(listed
                .unwrap_err()
                .to_string()
                .contains("invalid enrollment decision signature"));
        }
        Arc::new(core).shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn activation_setup_error_is_scoped_to_request_and_clears_after_conflict_removal() {
    let temp = tempfile::tempdir().unwrap();
    let core = start_unsupervised_core(temp.path().join("desktop")).await;
    let admin = PrincipalIdentity::load_or_create(&crate::client::paths::DesktopPaths::from_root(
        temp.path().join("admin"),
    ))
    .await
    .unwrap();
    let authority = signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
    commit_test_authority(&core, &authority).await;
    let mut conflict = crate::client::PeerRecord::new(
        "Existing peer",
        &authority.offer.server_ticket,
        "did:key:other-owner",
    );
    conflict.peer_id = authority.request.server_peer.clone();
    core.sync_state
        .upsert_for_test(conflict.clone())
        .await
        .unwrap();
    reconcile_with_test_transport(&core, test_transport(&authority, true)).await;
    let requests = core.active_status_enrollment_requests().await.unwrap();
    assert_eq!(requests[0].request_id, authority.request.request_id);
    assert_eq!(requests[0].activation_error.as_deref(), Some("route"));
    core.sync_state
        .queue_removal(&conflict, RemovalCause::Reconciliation)
        .await
        .unwrap()
        .unwrap();
    core.sync_state
        .complete_removal_if_matches(&conflict)
        .await
        .unwrap();
    reconcile_with_test_transport(&core, test_transport(&authority, true)).await;
    assert_eq!(
        core.active_status_enrollment_requests().await.unwrap()[0].activation_error,
        None
    );
    assert!(core
        .sync_state
        .snapshot()
        .enrollment_activation_errors
        .is_empty());
    Arc::new(core).shutdown().await.unwrap();
}

#[tokio::test]
async fn locally_removed_enrollment_stays_absent_across_reconcile() {
    use crate::client::paths::DesktopPaths;

    let temp = tempfile::tempdir().unwrap();
    let core = start_unsupervised_core(temp.path().join("desktop")).await;
    let admin =
        PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
            .await
            .unwrap();
    let authority = signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
    commit_admin_pin(&core, &authority).await;
    commit_enrollment_request(&core, &authority.request).await;
    commit_decision(&core, &authority).await;
    commit_revision(&core, &authority.revision).await;
    commit_route_receipt(&core, &authority).await;

    let transport = Arc::new(EnrollmentTransport {
        peer: authority.offer.server_ticket.clone(),
        resolved: Some(identity::Did::new(admin.did().to_string()).unwrap()),
        connected: false.into(),
        dials: 0.into(),
        observations: 0.into(),
    });
    let p2p: Arc<dyn P2POps> = transport.clone();
    let principal = Arc::new(core.node_identity().clone());
    let route_manager = Arc::new(ClientRouteManager::new(
        core.node_arc(),
        Arc::clone(&p2p),
        Arc::clone(&principal),
        core.options().operator_identity_interaction,
    ));
    let reconcile = || async {
        reconcile_status_enrollment_approvals(
            &core.node_arc(),
            &p2p,
            &principal,
            core.local_peer_id(),
            &core.sync_state,
            &route_manager,
        )
        .await
        .unwrap()
    };

    let authority_map = reconcile().await;
    assert!(
        authority_map.contains_key(&authority.request.server_peer),
        "the durable authority installs the enrolled peer: {authority_map:?}"
    );
    let installed = core.peer_records().await;
    assert_eq!(installed.len(), 1);
    assert_eq!(
        installed[0].enrollment_request_digest.as_deref(),
        Some(authority.request.request_digest.as_str())
    );

    core.sync_state
        .queue_removal(&installed[0], RemovalCause::Operator)
        .await
        .unwrap()
        .expect("removal is queued");

    reconcile().await;
    reconcile().await;
    assert!(
        core.peer_records().await.is_empty(),
        "a locally removed enrollment must stay absent while its lease still reads as current"
    );
    use std::sync::atomic::Ordering::SeqCst;
    assert_eq!(
        transport.dials.load(SeqCst),
        1,
        "a removed enrollment is not re-dialled by later reconciles"
    );
    assert_eq!(
        transport.observations.load(SeqCst),
        1,
        "a removed enrollment is filtered before the authentication stream"
    );
    assert!(
        core.active_status_enrollment_requests()
            .await
            .unwrap()
            .is_empty(),
        "a locally removed request is no longer listed"
    );
    assert_eq!(
        core.sync_state.retired_enrollment_digests().await,
        BTreeSet::from([authority.request.request_digest.clone()]),
        "the retirement lasts exactly as long as the durable authorization"
    );

    commit_revision(&core, &authority.revoked).await;
    reconcile().await;
    assert!(
        core.sync_state
            .retired_enrollment_digests()
            .await
            .is_empty(),
        "revocation removes the outcome and prunes the retirement with it"
    );
    assert!(core.peer_records().await.is_empty());

    Arc::new(core).shutdown().await.unwrap();
}

#[tokio::test]
async fn revoked_enrollment_ends_the_active_request_listing() {
    use crate::client::paths::DesktopPaths;

    let temp = tempfile::tempdir().unwrap();
    let core = start_unsupervised_core(temp.path().to_path_buf()).await;
    let admin =
        PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
            .await
            .unwrap();
    let authority = signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
    commit_admin_pin(&core, &authority).await;
    commit_enrollment_request(&core, &authority.request).await;
    commit_decision(&core, &authority).await;
    commit_revision(&core, &authority.revision).await;

    assert!(
        core.peer_records()
            .await
            .iter()
            .all(|record| !record.is_chat_ready_at(Utc::now())),
        "no configured peer is chat-ready before the route is finished"
    );
    let listed = core.active_status_enrollment_requests().await.unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].state, "approved");
    assert_eq!(listed[0].request_id, authority.request.request_id);

    commit_revision(&core, &authority.revoked).await;
    let listed = core.active_status_enrollment_requests().await.unwrap();
    assert!(
        listed.is_empty(),
        "a revoked authorization ends the request before its own expiry: {listed:?}"
    );

    Arc::new(core).shutdown().await.unwrap();
}

async fn assert_pending_visibility_matches_model(
    core: &ClientCore,
    request: &EnrollmentRequestRecord,
    case_name: &str,
) {
    let case = pending_request_cases()
        .into_iter()
        .find(|case| case.name == case_name)
        .expect("modeled enrollment case");
    let listed = core.active_status_enrollment_requests().await.unwrap();
    assert_eq!(
        listed
            .iter()
            .any(|row| row.request_id == request.request_id),
        !case.expected_satisfied,
        "{case_name}: {listed:?}",
    );
}

async fn configure_test_enrollment_route(
    core: &ClientCore,
    authority: &SignedEnrollmentAuthority,
    server_peer: &str,
) -> crate::client::peer_directory::PeerRecord {
    core.sync_state
        .upsert_enrollment_peer(
            server_peer,
            "Enrolled server",
            &authority.offer.server_ticket,
            &authority.request.owner_agent,
            &authority.request.network_id,
            &authority.request.request_id,
            &authority.request.request_digest,
            &authority.request.admin_did,
            authority.decision.authorization_sequence,
            &authority.decision.authorization_expires_at,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn reconnected_authority_satisfies_pending_request_until_a_newer_revision() {
    use crate::client::paths::DesktopPaths;

    let temp = tempfile::tempdir().unwrap();
    let core = start_unsupervised_core(temp.path().join("desktop")).await;
    let admin =
        PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
            .await
            .unwrap();
    let authority = signed_enrollment_authority_at(
        &admin,
        core.node_identity(),
        core.local_peer_id(),
        Utc::now() - chrono::Duration::minutes(10),
        "old",
    );
    let pending = signed_enrollment_authority_at(
        &admin,
        core.node_identity(),
        core.local_peer_id(),
        Utc::now(),
        "pending",
    );
    assert!(DateTime::parse_from_rfc3339(&authority.request.expires_at).unwrap() < Utc::now());
    assert!(
        DateTime::parse_from_rfc3339(&authority.decision.authorization_expires_at).unwrap()
            > Utc::now()
    );
    commit_admin_pin(&core, &authority).await;
    commit_enrollment_request(&core, &authority.request).await;
    commit_decision(&core, &authority).await;
    commit_revision(&core, &authority.revision).await;
    commit_route_receipt(&core, &authority).await;
    commit_enrollment_request(&core, &pending.request).await;
    let mut archived = signed_enrollment_authority_at(
        &admin,
        core.node_identity(),
        core.local_peer_id(),
        Utc::now() - chrono::Duration::minutes(20),
        "archived",
    );
    archived.decision.owner_agent = "unrelated-owner".into();
    archived.decision.admin_sig = admin.sign(&archived.decision.signing_payload()).unwrap();
    commit_enrollment_request(&core, &archived.request).await;
    commit_decision(&core, &archived).await;
    let before = graphql_with_transaction_retry(
        core.node(),
        STATUS_ENROLLMENT_QUERY,
        "capture enrollment documents before reconnect",
    )
    .await
    .unwrap();
    let configured =
        configure_test_enrollment_route(&core, &authority, &authority.request.server_peer).await;
    assert_pending_visibility_matches_model(&core, &pending.request, "route_still_reconnecting")
        .await;
    let ready = core
        .sync_state
        .set_pairing_ready(&configured, true)
        .await
        .unwrap()
        .unwrap();
    assert_pending_visibility_matches_model(
        &core,
        &pending.request,
        "reconnected_route_satisfies_pending_request",
    )
    .await;
    assert!(core
        .active_status_enrollment_requests()
        .await
        .unwrap()
        .is_empty());
    let after = graphql_with_transaction_retry(
        core.node(),
        STATUS_ENROLLMENT_QUERY,
        "capture enrollment documents after reconnect",
    )
    .await
    .unwrap();
    assert_eq!(
        before.data, after.data,
        "visibility must not mutate signed documents"
    );
    assert!(core
        .sync_state
        .retired_enrollment_digests()
        .await
        .is_empty());

    let mut newer = authority.revoked.clone();
    newer.kind = AuthorizationRevisionKind::Active;
    newer.revision_id = gents_protocol::enrollment::derive_revision_id(
        &newer.network_id,
        &newer.member_did,
        newer.sequence,
        &newer.kind,
        &newer.request_digest,
    );
    newer.admin_sig = admin.sign(&newer.signing_payload()).unwrap();
    commit_revision(&core, &newer).await;
    assert_eq!(core.sync_state.records(), vec![ready.clone()]);
    assert_pending_visibility_matches_model(
        &core,
        &pending.request,
        "revoked_or_expired_authorization",
    )
    .await;

    let mut revoked = authority.revoked.clone();
    revoked.sequence = 3;
    revoked.revision_id = gents_protocol::enrollment::derive_revision_id(
        &revoked.network_id,
        &revoked.member_did,
        revoked.sequence,
        &revoked.kind,
        &revoked.request_digest,
    );
    revoked.admin_sig = admin.sign(&revoked.signing_payload()).unwrap();
    commit_revision(&core, &revoked).await;
    assert_eq!(core.sync_state.records(), vec![ready]);
    assert_pending_visibility_matches_model(
        &core,
        &pending.request,
        "revoked_or_expired_authorization",
    )
    .await;
    Arc::new(core).shutdown().await.unwrap();
}

#[tokio::test]
async fn pending_request_requires_the_exact_current_applied_route() {
    use crate::client::paths::DesktopPaths;

    let temp = tempfile::tempdir().unwrap();
    let core = start_unsupervised_core(temp.path().join("desktop")).await;
    let admin =
        PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
            .await
            .unwrap();
    let authority = signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
    let pending = signed_enrollment_authority_at(
        &admin,
        core.node_identity(),
        core.local_peer_id(),
        Utc::now(),
        "pending",
    );
    commit_admin_pin(&core, &authority).await;
    commit_enrollment_request(&core, &authority.request).await;
    commit_decision(&core, &authority).await;
    commit_revision(&core, &authority.revision).await;
    commit_route_receipt(&core, &authority).await;
    commit_enrollment_request(&core, &pending.request).await;
    let unrelated = configure_test_enrollment_route(&core, &authority, "another-server").await;
    core.sync_state
        .set_pairing_ready(&unrelated, true)
        .await
        .unwrap()
        .unwrap();
    assert_pending_visibility_matches_model(&core, &pending.request, "route_still_reconnecting")
        .await;
    let configured =
        configure_test_enrollment_route(&core, &authority, &authority.request.server_peer).await;
    let mut ready = core
        .sync_state
        .set_pairing_ready(&configured, true)
        .await
        .unwrap()
        .unwrap();
    for field in [
        "node", "network", "profile", "request", "digest", "admin", "sequence", "expiry",
    ] {
        let mut mismatched = ready.clone();
        match field {
            "node" => mismatched.node_did = "another-node".into(),
            "network" => mismatched.pairing_network_id = Some("another-network".into()),
            "profile" => mismatched.pairing_template = Some("another-profile".into()),
            "request" => mismatched.enrollment_request_id = Some("another-request".into()),
            "digest" => mismatched.enrollment_request_digest = Some("another-digest".into()),
            "admin" => mismatched.enrollment_admin_did = Some("another-admin".into()),
            "sequence" => mismatched.enrollment_authorization_sequence = Some(2),
            "expiry" => {
                mismatched.enrollment_authorization_expires_at = Some(
                    (Utc::now() + chrono::Duration::hours(2))
                        .to_rfc3339_opts(SecondsFormat::Secs, true),
                )
            }
            _ => unreachable!(),
        }
        assert!(
            mismatched.is_chat_ready_at(Utc::now()),
            "cached readiness remains true: {field}"
        );
        let mismatched = core
            .sync_state
            .replace_record(&ready, mismatched)
            .await
            .unwrap()
            .unwrap();
        assert_pending_visibility_matches_model(
            &core,
            &pending.request,
            "route_still_reconnecting",
        )
        .await;
        ready = core
            .sync_state
            .replace_record(&mismatched, ready.clone())
            .await
            .unwrap()
            .unwrap();
    }
    assert_pending_visibility_matches_model(
        &core,
        &pending.request,
        "reconnected_route_satisfies_pending_request",
    )
    .await;
    Arc::new(core).shutdown().await.unwrap();
}

#[tokio::test]
async fn enrollment_challenge_waits_for_prior_route_teardown() {
    use crate::client::paths::DesktopPaths;
    use futures::FutureExt;
    use gents::runtime::agent::p2p_reconcile::read_client_replicated_schema;
    use gents_protocol::peer_schema::STATUS_REPLICATED_SCHEMA_FIELD;
    use std::sync::atomic::Ordering::SeqCst;

    let temp = tempfile::tempdir().unwrap();
    let mut core = start_unsupervised_core(temp.path().to_path_buf()).await;
    let admin =
        PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
            .await
            .unwrap();
    let authority = signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
    let transport = Arc::new(EnrollmentTransport {
        peer: authority.offer.server_peer.clone(),
        resolved: None,
        connected: false.into(),
        dials: 0.into(),
        observations: 0.into(),
    });
    core.p2p = transport.clone();
    let schema = read_client_replicated_schema(core.node_arc())
        .await
        .unwrap();
    let status = json!({
        "node_did":authority.offer.owner_agent,
        "enrollment":{"token":authority.offer_token},
        STATUS_REPLICATED_SCHEMA_FIELD:schema,
    });
    core.sync_state
        .compare_runtime_schema(&core.node, &authority.offer.owner_agent, &status)
        .await
        .unwrap();
    {
        let cleanup = core.route_manager.lock().await;
        let request = core.request_status_enrollment(&status);
        tokio::pin!(request);
        assert!(
            request.as_mut().now_or_never().is_none(),
            "an enrollment challenge raced the prior transport teardown"
        );
        assert_eq!(transport.dials.load(SeqCst), 0);
        assert_eq!(transport.observations.load(SeqCst), 0);
        drop(cleanup);
        let error = request.await.unwrap_err();
        assert!(format!("{error:#}").contains("identity challenge rejected"));
        assert_eq!(transport.dials.load(SeqCst), 1);
        assert_eq!(transport.observations.load(SeqCst), 1);
    }
    Arc::new(core).shutdown().await.unwrap();
}

#[tokio::test]
async fn a_locally_retired_request_refuses_its_offer_instead_of_sharing_its_challenge() {
    use crate::client::paths::DesktopPaths;

    let temp = tempfile::tempdir().unwrap();
    let core = start_unsupervised_core(temp.path().to_path_buf()).await;
    let admin =
        PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
            .await
            .unwrap();
    let authority = signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
    commit_enrollment_request(&core, &authority.request).await;
    locally_retire(&core, &authority).await;

    let offer = decode_offer(&authority.offer_token).unwrap();
    let error = core
        .existing_request_for_offer(&offer, &authority.offer_token, core.local_peer_id())
        .await
        .expect_err("a retired request must not be reused or replaced under its offer");
    assert!(
        error.to_string().contains("fetch a fresh offer"),
        "{error:#}"
    );

    Arc::new(core).shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_authority_rows_are_skipped_by_the_active_listing() {
    use crate::client::paths::DesktopPaths;

    let temp = tempfile::tempdir().unwrap();
    let core = start_unsupervised_core(temp.path().to_path_buf()).await;
    let admin =
        PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
            .await
            .unwrap();
    let authority = signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
    commit_admin_pin(&core, &authority).await;
    commit_enrollment_request(&core, &authority.request).await;
    commit_decision(&core, &authority).await;
    commit_revision(&core, &authority.revision).await;

    let mut malformed_revision = authority.revision.clone();
    malformed_revision.revision_id = format!("{}-malformed", malformed_revision.revision_id);
    commit_revision_with_admin_sig(&core, &malformed_revision, "not-a-base58-signature").await;
    let mut foreign = authority.request.clone();
    foreign.request_id = format!("{}-foreign", foreign.request_id);
    foreign.request_digest = format!("{}-foreign", foreign.request_digest);
    foreign.challenge = format!("{}-foreign", foreign.challenge);
    foreign.candidate_did = admin.did().to_string();
    foreign.candidate_peer = "foreign-peer".into();
    foreign.candidate_sig = vec![1, 2, 3];
    commit_enrollment_request(&core, &foreign).await;

    let listed = core.active_status_enrollment_requests().await.unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].state, "approved");
    assert_eq!(listed[0].request_id, authority.request.request_id);

    Arc::new(core).shutdown().await.unwrap();
}

#[tokio::test]
async fn a_saved_peer_removed_for_an_absent_receipt_reinstalls_when_it_returns() {
    use crate::client::paths::DesktopPaths;

    let temp = tempfile::tempdir().unwrap();
    let core = start_unsupervised_core(temp.path().to_path_buf()).await;
    let admin =
        PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
            .await
            .unwrap();
    let authority = signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
    commit_admin_pin(&core, &authority).await;
    commit_enrollment_request(&core, &authority.request).await;
    commit_decision(&core, &authority).await;
    commit_revision(&core, &authority.revision).await;
    core.sync_state
        .upsert_enrollment_peer(
            &authority.request.server_peer,
            "Enrolled server",
            &authority.offer.server_ticket,
            &authority.request.owner_agent,
            &authority.request.network_id,
            &authority.request.request_id,
            &authority.request.request_digest,
            &authority.request.admin_did,
            authority.decision.authorization_sequence,
            &authority.decision.authorization_expires_at,
        )
        .await
        .unwrap();

    let transport = Arc::new(EnrollmentTransport {
        peer: authority.offer.server_ticket.clone(),
        resolved: Some(identity::Did::new(admin.did().to_string()).unwrap()),
        connected: false.into(),
        dials: 0.into(),
        observations: 0.into(),
    });
    let p2p: Arc<dyn P2POps> = transport.clone();
    let principal = Arc::new(core.node_identity().clone());
    let route_manager = Arc::new(ClientRouteManager::new(
        core.node_arc(),
        Arc::clone(&p2p),
        Arc::clone(&principal),
        core.options().operator_identity_interaction,
    ));
    let reconcile = || async {
        reconcile_status_enrollment_approvals(
            &core.node_arc(),
            &p2p,
            &principal,
            core.local_peer_id(),
            &core.sync_state,
            &route_manager,
        )
        .await
        .unwrap()
    };

    reconcile().await;
    assert!(
        core.peer_records().await.is_empty(),
        "reconciliation tears down a saved peer whose route receipt is unobserved"
    );
    assert!(
        core.sync_state
            .retired_enrollment_digests()
            .await
            .is_empty(),
        "an automatic teardown must not retire a generation the operator kept"
    );

    commit_route_receipt(&core, &authority).await;
    let authority_map = reconcile().await;
    assert!(
        authority_map.contains_key(&authority.request.server_peer),
        "the restored receipt reinstalls the enrolled server: {authority_map:?}"
    );
    let installed = core.peer_records().await;
    assert_eq!(
        installed
            .iter()
            .map(|record| record.enrollment_request_digest.as_deref())
            .collect::<Vec<_>>(),
        vec![Some(authority.request.request_digest.as_str())]
    );

    Arc::new(core).shutdown().await.unwrap();
}

#[tokio::test]
async fn a_retirement_survives_an_absent_route_receipt_while_authority_is_current() {
    use crate::client::paths::DesktopPaths;
    use std::sync::atomic::Ordering::SeqCst;

    let temp = tempfile::tempdir().unwrap();
    let core = start_unsupervised_core(temp.path().to_path_buf()).await;
    let admin =
        PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
            .await
            .unwrap();
    let authority = signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
    commit_admin_pin(&core, &authority).await;
    commit_enrollment_request(&core, &authority.request).await;
    commit_decision(&core, &authority).await;
    commit_revision(&core, &authority.revision).await;
    locally_retire(&core, &authority).await;

    let transport = Arc::new(EnrollmentTransport {
        peer: authority.offer.server_ticket.clone(),
        resolved: Some(identity::Did::new(admin.did().to_string()).unwrap()),
        connected: false.into(),
        dials: 0.into(),
        observations: 0.into(),
    });
    let p2p: Arc<dyn P2POps> = transport.clone();
    let principal = Arc::new(core.node_identity().clone());
    let route_manager = Arc::new(ClientRouteManager::new(
        core.node_arc(),
        Arc::clone(&p2p),
        Arc::clone(&principal),
        core.options().operator_identity_interaction,
    ));
    let reconcile = || async {
        reconcile_status_enrollment_approvals(
            &core.node_arc(),
            &p2p,
            &principal,
            core.local_peer_id(),
            &core.sync_state,
            &route_manager,
        )
        .await
        .unwrap()
    };

    reconcile().await;
    assert_eq!(
        core.sync_state.retired_enrollment_digests().await,
        BTreeSet::from([authority.request.request_digest.clone()]),
        "an absent route receipt must not prune a retirement whose authority is current"
    );
    assert!(core.peer_records().await.is_empty());

    commit_route_receipt(&core, &authority).await;
    reconcile().await;
    assert_eq!(
        core.sync_state.retired_enrollment_digests().await,
        BTreeSet::from([authority.request.request_digest.clone()]),
        "the retirement outlives the authorization generation it names"
    );
    assert!(
        core.peer_records().await.is_empty(),
        "a receipt arriving after removal must not reinstall the enrolled server"
    );
    assert_eq!(
        transport.dials.load(SeqCst),
        0,
        "a removed enrollment is not re-dialled once its receipt returns"
    );
    assert_eq!(transport.observations.load(SeqCst), 0);

    Arc::new(core).shutdown().await.unwrap();
}
