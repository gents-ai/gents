use super::{
    canonical_enrollment_digest, canonical_enrollment_payload, frame_enrollment_field,
    AuthorizationRevision, AuthorizationRevisionKind, DurableEnrollmentDocuments, EnrollmentAction,
    EnrollmentDecision, EnrollmentDecisionKind, EnrollmentOffer, EnrollmentRequest,
    EnrollmentRouteDirection, EnrollmentRouteReceipt, EnrollmentState, NetworkAdminPin,
};
use gents_lean_vocab::families::enrollment_core_trace::{StepInput, StepObservation};

fn lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn offer(step: &StepInput) -> EnrollmentOffer {
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

fn request(step: &StepInput) -> EnrollmentRequest {
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

fn decision(step: &StepInput) -> EnrollmentDecision {
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
        authorization_sequence: step.decision_authorization_sequence as usize,
        authorization_expires_at: step.decision_authorization_expires_at.clone(),
        signer_did: step.decision_signer_did.clone(),
        admin_signed: step.decision_admin_signed,
        fresh: step.decision_fresh,
    }
}

fn revision(step: &StepInput) -> AuthorizationRevision {
    AuthorizationRevision {
        request_id: step.revision_request_id.clone(),
        request_digest: step.revision_request_digest.clone(),
        network_id: step.revision_network_id.clone(),
        admin_did: step.revision_admin_did.clone(),
        member_did: step.revision_member_did.clone(),
        member_peer: step.revision_member_peer.clone(),
        owner_node: step.revision_owner_node.clone(),
        sequence: step.revision_sequence as usize,
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

fn receipt(step: &StepInput) -> EnrollmentRouteReceipt {
    EnrollmentRouteReceipt {
        request_id: step.receipt_request_id.clone(),
        request_digest: step.receipt_request_digest.clone(),
        network_id: step.receipt_network_id.clone(),
        admin_did: step.receipt_admin_did.clone(),
        member_did: step.receipt_member_did.clone(),
        member_peer: step.receipt_member_peer.clone(),
        server_peer: step.receipt_server_peer.clone(),
        owner_node: step.receipt_owner_node.clone(),
        authorization_sequence: step.receipt_authorization_sequence as usize,
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

fn apply_step(state: &mut EnrollmentState, step: &StepInput) {
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

fn observe_step(state: &EnrollmentState, step: &StepInput) -> StepObservation {
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
    let offer = has_offer.then(|| offer(step));
    let request = has_request.then(|| request(step));
    let decision = (has_request && has_decision).then(|| decision(step));
    let approved = request.as_ref().zip(decision.as_ref());
    StepObservation {
        observed_offer_count: state.observed_offer_count() as u64,
        admin_pin_count: state.admin_pin_count() as u64,
        challenge_binding_count: state.challenge_binding_count() as u64,
        request_binding_count: state.request_binding_count() as u64,
        request_count: state.accepted_requests.len() as u64,
        decision_count: state.decisions.len() as u64,
        authorization_count: state.authorizations.len() as u64,
        membership_count: state.memberships.len() as u64,
        receipt_count: state.route_receipts.len() as u64,
        route_count: state.applied_routes.len() as u64,
        request_accepted: request
            .as_ref()
            .is_some_and(|request| state.accepted_requests.contains(request)),
        decision_recorded: decision
            .as_ref()
            .is_some_and(|decision| state.decisions.contains(decision)),
        authorization_recorded: approved.is_some_and(|(request, decision)| {
            state
                .authorizations
                .contains(&EnrollmentState::revision_for_approval(request, decision))
        }),
        revision_recorded: (has_request && has_revision)
            && state.authorizations.contains(&revision(step)),
        receipt_recorded: (has_request && has_receipt)
            && state.route_receipts.contains(&receipt(step)),
        membership_present: approved.is_some_and(|(request, decision)| {
            state
                .memberships
                .contains(&EnrollmentState::membership_for(request, decision))
        }),
        client_route_present: approved.is_some_and(|(request, decision)| {
            state
                .applied_routes
                .contains(&EnrollmentState::client_route(request, decision))
        }),
        server_route_present: approved.is_some_and(|(request, decision)| {
            state
                .applied_routes
                .contains(&EnrollmentState::server_route(request, decision))
        }),
        admin_pin_present: offer
            .as_ref()
            .is_some_and(|offer| state.admin_pin_present(offer)),
        admin_pin_conflict: offer
            .as_ref()
            .is_some_and(|offer| state.admin_pin_conflict(offer)),
        challenge_binding_conflict: request
            .as_ref()
            .is_some_and(|request| state.challenge_binding_conflict(request)),
        request_binding_conflict: request
            .as_ref()
            .is_some_and(|request| state.request_binding_conflict(request)),
        current_approval: approved
            .is_some_and(|(request, decision)| state.current_approval(request, decision)),
        peer_admitted: state.peer_operationally_authorized(&step.peer_admission_did),
        ready: request
            .as_ref()
            .is_some_and(|request| state.enrollment_ready(request)),
    }
}

#[test]
fn generated_enrollment_cases_match_production_transition_core() {
    gents_lean_vocab::families::enrollment_core_trace::replay(|case| {
        let mut state = EnrollmentState::default();
        case.steps
            .iter()
            .map(|step| {
                apply_step(&mut state, step);
                observe_step(&state, step)
            })
            .collect()
    });
}

#[test]
fn generated_enrollment_durable_cases_drive_current_projection() {
    gents_lean_vocab::families::enrollment_core_durable_projection::replay(|case| {
        let steps: Vec<StepInput> = gents_lean_vocab::family::recode(&case.documents);
        let mut documents = DurableEnrollmentDocuments::default();
        for step in &steps {
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
        let first = |action: &str| {
            steps
                .iter()
                .find(|step| step.action == action)
                .unwrap_or_else(|| panic!("generated durable case {} has no {action}", case.name))
        };
        let offer = offer(first("observe_offer"));
        let request = request(first("accept_request"));
        let decision = decision(first("approve_request"));
        let route_receipt = receipt(first("record_server_route_receipt"));
        Some(
            gents_lean_vocab::families::enrollment_core_durable_projection::DurableProjection {
                current_approval: documents.current_approval(&offer, &request, &decision),
                current_route_receipt: documents.current_server_route_receipt(
                    &offer,
                    &request,
                    &decision,
                    &route_receipt,
                ),
            },
        )
    });
}

#[test]
fn generated_enrollment_encoding_vectors_match_wire_codec() {
    gents_lean_vocab::families::enrollment_core_frame::replay(|case| {
        gents_lean_vocab::families::enrollment_core_frame::Frame {
            frame: lower_hex(&frame_enrollment_field(&case.value)),
        }
    });
}

#[test]
fn generated_enrollment_digest_vectors_match_wire_codec() {
    gents_lean_vocab::families::enrollment_core_digest::replay(|case| {
        gents_lean_vocab::families::enrollment_core_digest::CanonicalDigest {
            payload: lower_hex(&canonical_enrollment_payload(
                case.fields.iter().map(String::as_str),
            )),
            digest: canonical_enrollment_digest(case.fields.iter().map(String::as_str)),
        }
    });
}
