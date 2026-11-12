use super::{canonical_enrollment_digest, canonical_enrollment_payload, frame_enrollment_field, AuthorizationRevision, AuthorizationRevisionKind, DurableEnrollmentDocuments, EnrollmentAction, EnrollmentDecision, EnrollmentDecisionKind, EnrollmentOffer, EnrollmentRequest, EnrollmentRouteDirection, EnrollmentRouteReceipt, EnrollmentState, NetworkAdminPin};
use gents_lean_vocab::{lean_enrollment_cases, lean_enrollment_digest_cases, lean_enrollment_durable_projection_cases, lean_enrollment_encoding_cases, LeanEnrollmentTraceStep};

fn lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn offer(step: &LeanEnrollmentTraceStep) -> EnrollmentOffer {
    EnrollmentOffer {
        offer_id: step.offer_id.clone(),
        challenge: step.offer_challenge.clone(),
        network_id: step.offer_network_id.clone(),
        admin_did: step.offer_admin_did.clone(),
        server_peer: step.offer_server_peer.clone(),
        server_ticket_peer: step.server_ticket_peer.clone(),
        resolved_server_did: step.resolved_server_did.clone(),
        owner_node: step.offer_owner_node.clone(),
        profile: step.offer_profile.clone(),
        schema_compatible: step.schema_compatible,
        admin_signed: step.offer_admin_signed,
        fresh: step.offer_fresh,
    }
}

fn request(step: &LeanEnrollmentTraceStep) -> EnrollmentRequest {
    EnrollmentRequest {
        request_id: step.request_id.clone(),
        digest: step.request_digest.clone(),
        offer_id: step.request_offer_id.clone(),
        challenge: step.challenge.clone(),
        network_id: step.network_id.clone(),
        admin_did: step.admin_did.clone(),
        server_peer: step.server_peer.clone(),
        candidate_did: step.candidate_did.clone(),
        candidate_peer: step.candidate_peer.clone(),
        candidate_name: None,
        observed_candidate_peer: step.observed_candidate_peer.clone(),
        resolved_candidate_did: step.resolved_candidate_did.clone(),
        candidate_ticket_peer: step.candidate_ticket_peer.clone(),
        owner_node: step.owner_node.clone(),
        profile: step.profile.clone(),
        client_nonce: step.client_nonce.clone(),
        issued_at: step.issued_at.clone(),
        expires_at: step.expires_at.clone(),
        candidate_signed: step.candidate_signed,
        fresh: step.request_fresh,
    }
}

fn decision(step: &LeanEnrollmentTraceStep) -> EnrollmentDecision {
    EnrollmentDecision {
        request_id: step.decision_request_id.clone(),
        request_digest: step.decision_request_digest.clone(),
        network_id: step.decision_network_id.clone(),
        admin_did: step.decision_admin_did.clone(),
        candidate_did: step.decision_candidate_did.clone(),
        candidate_peer: step.decision_candidate_peer.clone(),
        owner_node: step.decision_owner_node.clone(),
        kind: match step.decision_kind.as_str() {
            "approved" => EnrollmentDecisionKind::Approved,
            "denied" => EnrollmentDecisionKind::Denied,
            other => panic!("unknown generated enrollment decision kind {other:?}"),
        },
        authorization_sequence: step.decision_authorization_sequence,
        authorization_expires_at: step.decision_authorization_expires_at.clone(),
        signer_did: step.decision_signer_did.clone(),
        admin_signed: step.decision_admin_signed,
        fresh: step.decision_fresh,
    }
}

fn revision(step: &LeanEnrollmentTraceStep) -> AuthorizationRevision {
    AuthorizationRevision {
        request_id: step.revision_request_id.clone(),
        request_digest: step.revision_request_digest.clone(),
        network_id: step.revision_network_id.clone(),
        admin_did: step.revision_admin_did.clone(),
        member_did: step.revision_member_did.clone(),
        member_peer: step.revision_member_peer.clone(),
        owner_node: step.revision_owner_node.clone(),
        sequence: step.revision_sequence,
        authorization_expires_at: step.revision_authorization_expires_at.clone(),
        kind: match step.revision_kind.as_str() {
            "active" => AuthorizationRevisionKind::Active,
            "revoked" => AuthorizationRevisionKind::Revoked,
            other => panic!("unknown generated enrollment revision kind {other:?}"),
        },
        signer_did: step.revision_signer_did.clone(),
        admin_signed: step.revision_admin_signed,
    }
}

fn receipt(step: &LeanEnrollmentTraceStep) -> EnrollmentRouteReceipt {
    EnrollmentRouteReceipt {
        request_id: step.receipt_request_id.clone(),
        request_digest: step.receipt_request_digest.clone(),
        network_id: step.receipt_network_id.clone(),
        admin_did: step.receipt_admin_did.clone(),
        member_did: step.receipt_member_did.clone(),
        member_peer: step.receipt_member_peer.clone(),
        server_peer: step.receipt_server_peer.clone(),
        owner_node: step.receipt_owner_node.clone(),
        authorization_sequence: step.receipt_authorization_sequence,
        authorization_expires_at: step.receipt_authorization_expires_at.clone(),
        direction: match step.receipt_direction.as_str() {
            "client_to_server" => EnrollmentRouteDirection::ClientToServer,
            "server_to_client" => EnrollmentRouteDirection::ServerToClient,
            other => panic!("unknown generated enrollment receipt direction {other:?}"),
        },
        signer_did: step.receipt_signer_did.clone(),
        admin_signed: step.receipt_admin_signed,
        applied: step.receipt_applied,
    }
}

fn apply_step(state: &mut EnrollmentState, step: &LeanEnrollmentTraceStep) {
    let request = request(step);
    let action = match step.action.as_str() {
        "observe_offer" => EnrollmentAction::ObserveOffer(offer(step)),
        "confirm_admin_pin" => EnrollmentAction::ConfirmAdminPin(offer(step)),
        "accept_request" => EnrollmentAction::AcceptRequest(offer(step), request.clone()),
        "approve_request" | "deny_request" => {
            EnrollmentAction::DecideRequest(request.clone(), decision(step))
        }
        "materialize_membership" => {
            EnrollmentAction::MaterializeMembership(request.clone(), decision(step))
        }
        "materialize_client_route" => {
            EnrollmentAction::MaterializeClientRoute(request.clone(), decision(step))
        }
        "record_server_route_receipt" => EnrollmentAction::RecordServerRouteReceipt(
            request.clone(),
            decision(step),
            receipt(step),
        ),
        "revoke_membership" => EnrollmentAction::Revoke(request.clone(), revision(step)),
        "merge_authorization" => EnrollmentAction::MergeAuthorization(revision(step)),
        other => panic!("unknown generated enrollment action {other:?}"),
    };
    state.apply(action);
}

fn assert_step(state: &EnrollmentState, step: &LeanEnrollmentTraceStep, context: &str) {
    assert_eq!(
        state.observed_offer_count(),
        step.observed_offer_count,
        "{context}"
    );
    assert_eq!(state.admin_pin_count(), step.admin_pin_count, "{context}");
    assert_eq!(
        state.challenge_binding_count(),
        step.challenge_binding_count,
        "{context}"
    );
    assert_eq!(
        state.request_binding_count(),
        step.request_binding_count,
        "{context}"
    );
    assert_eq!(
        state.accepted_requests.len(),
        step.request_count,
        "{context}"
    );
    assert_eq!(state.decisions.len(), step.decision_count, "{context}");
    assert_eq!(
        state.authorizations.len(),
        step.authorization_count,
        "{context}"
    );
    assert_eq!(state.memberships.len(), step.membership_count, "{context}");
    assert_eq!(state.route_receipts.len(), step.receipt_count, "{context}");
    assert_eq!(state.applied_routes.len(), step.route_count, "{context}");
    assert_eq!(
        state.peer_operationally_authorized(&step.peer_admission_did),
        step.peer_admitted,
        "{context}"
    );

    let has_offer = matches!(
        step.action.as_str(),
        "observe_offer" | "confirm_admin_pin" | "accept_request"
    );
    let has_request = !matches!(step.action.as_str(), "observe_offer" | "confirm_admin_pin");
    let has_decision = matches!(
        step.action.as_str(),
        "approve_request"
            | "deny_request"
            | "materialize_membership"
            | "materialize_client_route"
            | "record_server_route_receipt"
            | "merge_authorization"
    );
    let has_revision = matches!(
        step.action.as_str(),
        "revoke_membership" | "merge_authorization"
    );
    let has_receipt = step.action == "record_server_route_receipt";

    if has_offer {
        let offer = offer(step);
        assert_eq!(
            state.admin_pin_present(&offer),
            step.admin_pin_present,
            "{context}"
        );
        assert_eq!(
            state.admin_pin_conflict(&offer),
            step.admin_pin_conflict,
            "{context}"
        );
    } else {
        assert!(!step.admin_pin_present, "{context}");
        assert!(!step.admin_pin_conflict, "{context}");
    }

    if !has_request {
        assert!(!step.request_accepted, "{context}");
        assert!(!step.challenge_binding_conflict, "{context}");
        assert!(!step.request_binding_conflict, "{context}");
        assert!(!step.decision_recorded, "{context}");
        assert!(!step.authorization_recorded, "{context}");
        assert!(!step.revision_recorded, "{context}");
        assert!(!step.receipt_recorded, "{context}");
        assert!(!step.membership_present, "{context}");
        assert!(!step.client_route_present, "{context}");
        assert!(!step.server_route_present, "{context}");
        assert!(!step.current_approval, "{context}");
        assert!(!step.ready, "{context}");
        return;
    }

    let request = request(step);
    assert_eq!(
        state.accepted_requests.contains(&request),
        step.request_accepted,
        "{context}"
    );
    assert_eq!(
        state.challenge_binding_conflict(&request),
        step.challenge_binding_conflict,
        "{context}"
    );
    assert_eq!(
        state.request_binding_conflict(&request),
        step.request_binding_conflict,
        "{context}"
    );
    assert_eq!(state.enrollment_ready(&request), step.ready, "{context}");

    if has_decision {
        let decision = decision(step);
        let authorization = EnrollmentState::revision_for_approval(&request, &decision);
        assert_eq!(
            state.decisions.contains(&decision),
            step.decision_recorded,
            "{context}"
        );
        assert_eq!(
            state.authorizations.contains(&authorization),
            step.authorization_recorded,
            "{context}"
        );
        assert_eq!(
            state
                .memberships
                .contains(&EnrollmentState::membership_for(&request, &decision)),
            step.membership_present,
            "{context}"
        );
        assert_eq!(
            state
                .applied_routes
                .contains(&EnrollmentState::client_route(&request, &decision)),
            step.client_route_present,
            "{context}"
        );
        assert_eq!(
            state
                .applied_routes
                .contains(&EnrollmentState::server_route(&request, &decision)),
            step.server_route_present,
            "{context}"
        );
        assert_eq!(
            state.current_approval(&request, &decision),
            step.current_approval,
            "{context}"
        );
    } else {
        assert!(!step.decision_recorded, "{context}");
        assert!(!step.authorization_recorded, "{context}");
        assert!(!step.membership_present, "{context}");
        assert!(!step.client_route_present, "{context}");
        assert!(!step.server_route_present, "{context}");
        assert!(!step.current_approval, "{context}");
    }

    if has_revision {
        assert_eq!(
            state.authorizations.contains(&revision(step)),
            step.revision_recorded,
            "{context}"
        );
    } else {
        assert!(!step.revision_recorded, "{context}");
    }
    if has_receipt {
        assert_eq!(
            state.route_receipts.contains(&receipt(step)),
            step.receipt_recorded,
            "{context}"
        );
    } else {
        assert!(!step.receipt_recorded, "{context}");
    }
}

#[test]
fn generated_enrollment_cases_match_production_transition_core() {
    let cases = lean_enrollment_cases();
    assert_eq!(cases.len(), 36);
    for case in cases {
        let mut state = EnrollmentState::default();
        assert!(!case.steps.is_empty(), "{}", case.name);
        for (index, step) in case.steps.iter().enumerate() {
            apply_step(&mut state, step);
            assert_step(&state, step, &format!("{} step {index}", case.name));
        }
    }
}

#[test]
fn generated_enrollment_durable_cases_drive_current_projection() {
    let cases = lean_enrollment_durable_projection_cases();
    assert_eq!(cases.len(), 6);
    for case in cases {
        let mut documents = DurableEnrollmentDocuments::default();
        for step in &case.documents {
            match step.action.as_str() {
                "observe_offer" => {
                    documents.offers.insert(offer(step));
                }
                "confirm_admin_pin" => {
                    let offer = offer(step);
                    documents.admin_pins.insert(NetworkAdminPin {
                        network_id: offer.network_id,
                        admin_did: offer.admin_did,
                    });
                }
                "accept_request" => {
                    documents.requests.insert(request(step));
                }
                "approve_request" | "deny_request" => {
                    documents.decisions.insert(decision(step));
                }
                "merge_authorization" => {
                    documents.revisions.insert(revision(step));
                }
                "record_server_route_receipt" => {
                    documents.route_receipts.insert(receipt(step));
                }
                other => panic!("unknown durable enrollment document action {other:?}"),
            }
        }
        let offer = case
            .documents
            .iter()
            .find(|step| step.action == "observe_offer")
            .map(offer)
            .expect("generated durable case must name the observed offer");
        let request = case
            .documents
            .iter()
            .find(|step| step.action == "accept_request")
            .map(request)
            .expect("generated durable case must name the target request");
        let decision = case
            .documents
            .iter()
            .find(|step| step.action == "approve_request")
            .map(decision)
            .expect("generated durable case must name the target approval");
        let route_receipt = case
            .documents
            .iter()
            .find(|step| step.action == "record_server_route_receipt")
            .map(receipt)
            .expect("generated durable case must name the target route receipt");
        assert_eq!(
            documents.current_approval(&offer, &request, &decision),
            case.expected_current_approval,
            "{}",
            case.name
        );
        assert_eq!(
            documents.current_server_route_receipt(&offer, &request, &decision, &route_receipt,),
            case.expected_current_route_receipt,
            "{} route receipt",
            case.name
        );
    }
}

#[test]
fn generated_enrollment_encoding_vectors_match_wire_codec() {
    let cases = lean_enrollment_encoding_cases();
    assert_eq!(cases.len(), 4);
    for case in cases {
        let actual = lower_hex(&frame_enrollment_field(&case.value));
        assert_eq!(actual, case.expected_frame, "{}", case.name);
    }
}

#[test]
fn generated_enrollment_digest_vectors_match_wire_codec() {
    let cases = lean_enrollment_digest_cases();
    assert_eq!(cases.len(), 2);
    for case in cases {
        let actual_payload = lower_hex(&canonical_enrollment_payload(
            case.fields.iter().map(String::as_str),
        ));
        let actual_digest = canonical_enrollment_digest(case.fields.iter().map(String::as_str));
        assert_eq!(actual_payload, case.expected_payload, "{}", case.name);
        assert_eq!(actual_digest, case.expected_digest, "{}", case.name);
    }
}
