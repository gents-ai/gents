//! Conformance fence for `Proofs/Enrollment`.

use gents::agent::p2p_reconcile::enrollment::{
    canonical_enrollment_digest, canonical_enrollment_payload, frame_enrollment_field,
    AuthorizationRevision, AuthorizationRevisionKind, DurableEnrollmentDocuments, EnrollmentAction,
    EnrollmentDecision, EnrollmentDecisionKind, EnrollmentOffer, EnrollmentRequest,
    EnrollmentRouteDirection, EnrollmentRouteReceipt, EnrollmentState, NetworkAdminPin,
};
use gents_protocol::request_admission::{
    project_agent_request_admission, project_agent_request_admission_disposition,
    AgentRequestAdmissionKind, AgentRequestAdmissionObservation, RuntimeInternalSourceKind,
};

use crate::lean_vocab_test::{
    lean_agent_request_admission_cases, lean_enrollment_cases, lean_enrollment_digest_cases,
    lean_enrollment_durable_projection_cases, lean_enrollment_encoding_cases,
    LeanEnrollmentTraceStep,
};

#[test]
fn generated_agent_request_admission_cases_match_shared_projector() {
    let cases = lean_agent_request_admission_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no AgentRequest admission cases"
    );
    for case in cases {
        let kind = AgentRequestAdmissionKind::try_from(case.kind.as_str())
            .unwrap_or_else(|error| panic!("{}: {error}: {:?}", case.name, case.kind));
        let runtime_source_kind = RuntimeInternalSourceKind::try_from(
            case.runtime_source_kind.as_str(),
        )
        .unwrap_or_else(|error| panic!("{}: {error}: {:?}", case.name, case.runtime_source_kind));
        let observation = AgentRequestAdmissionObservation {
            kind,
            signature_valid: case.signature_valid,
            signed_fields_match: case.signed_fields_match,
            branch_fields_exact: case.branch_fields_exact,
            pending_deadline_absent: case.pending_deadline_absent,
            signer_matches_requester: case.signer_matches_requester,
            requester_matches_target: case.requester_matches_target,
            signer_matches_target: case.signer_matches_target,
            signer_matches_issuer: case.signer_matches_issuer,
            requester_matches_issuer: case.requester_matches_issuer,
            current_approval: case.current_approval,
            exact_generation: case.exact_generation,
            authorization_fresh: case.authorization_fresh,
            runtime_evidence_present: case.runtime_evidence_present,
            runtime_source_kind,
            target_runtime_attestation_valid: case.target_runtime_attestation_valid,
            source_binding_current: case.source_binding_current,
            trigger_config_document_binding_current: case.trigger_config_document_binding_current,
            source_document_binding_current: case.source_document_binding_current,
            source_tool_call_binding_current: case.source_tool_call_binding_current,
            target_policy_allows: case.target_policy_allows,
            bridge_author_binding_current: case.bridge_author_binding_current,
            bridge_author_authorization_fresh: case.bridge_author_authorization_fresh,
            target_cross_deployment_policy_allows: case.target_cross_deployment_policy_allows,
        };
        let actual = project_agent_request_admission(observation);
        assert_eq!(actual, case.expected_admitted, "{}", case.name);
        assert_eq!(
            project_agent_request_admission_disposition(case.observation_available, observation)
                .as_str(),
            case.expected_disposition,
            "{}",
            case.name
        );
    }
}

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
        owner_agent: step.offer_owner_agent.clone(),
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
        observed_candidate_peer: step.observed_candidate_peer.clone(),
        resolved_candidate_did: step.resolved_candidate_did.clone(),
        candidate_ticket_peer: step.candidate_ticket_peer.clone(),
        owner_agent: step.owner_agent.clone(),
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
        owner_agent: step.decision_owner_agent.clone(),
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
        owner_agent: step.revision_owner_agent.clone(),
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
        owner_agent: step.receipt_owner_agent.clone(),
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
        "observe_legacy_pairing_desired" => {
            EnrollmentAction::ObserveLegacyPairingDesired(step.peer_admission_did.clone())
        }
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
    let has_request = !matches!(
        step.action.as_str(),
        "observe_legacy_pairing_desired" | "observe_offer" | "confirm_admin_pin"
    );
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
        assert!(!step.client_hydration_admits, "{context}");
        assert!(!step.server_hydration_admits, "{context}");
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
    assert_eq!(
        state.hydration_admits(&request, EnrollmentRouteDirection::ClientToServer),
        step.client_hydration_admits,
        "{context}"
    );
    assert_eq!(
        state.hydration_admits(&request, EnrollmentRouteDirection::ServerToClient),
        step.server_hydration_admits,
        "{context}"
    );

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
    assert_eq!(cases.len(), 37);
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
fn generated_enrollment_durable_projection_cases_are_order_independent() {
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

        let mut reversed = documents.clone();
        reversed.offers = documents.offers.iter().rev().cloned().collect();
        reversed.requests = documents.requests.iter().rev().cloned().collect();
        reversed.decisions = documents.decisions.iter().rev().cloned().collect();
        reversed.revisions = documents.revisions.iter().rev().cloned().collect();
        reversed.route_receipts = documents.route_receipts.iter().rev().cloned().collect();
        assert_eq!(
            reversed.current_approval(&offer, &request, &decision),
            case.expected_current_approval,
            "{} reversed",
            case.name
        );
        assert_eq!(
            reversed.current_server_route_receipt(&offer, &request, &decision, &route_receipt,),
            case.expected_current_route_receipt,
            "{} reversed route receipt",
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
        assert!(case.frame_matches, "{}", case.name);
        assert_eq!(case.actual_frame, case.expected_frame, "{}", case.name);
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
        assert!(case.payload_matches, "{}", case.name);
        assert!(case.digest_matches, "{}", case.name);
        assert_eq!(case.actual_payload, case.expected_payload, "{}", case.name);
        assert_eq!(case.actual_digest, case.expected_digest, "{}", case.name);
        assert_eq!(actual_payload, case.expected_payload, "{}", case.name);
        assert_eq!(actual_digest, case.expected_digest, "{}", case.name);
    }
}
