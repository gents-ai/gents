use gents_protocol::request_admission::{
    AgentRequestAdmissionDisposition, AgentRequestAdmissionKind, AgentRequestAdmissionObservation,
    RuntimeInternalSourceKind,
};

use super::final_claim_admission_disposition;

#[test]
fn admission_projection_follows_the_lean_model() {
    let admitted_kinds = std::cell::RefCell::new(std::collections::BTreeSet::new());
    let refused_over_hop_kinds = std::cell::RefCell::new(std::collections::BTreeSet::new());
    gents_lean_vocab::families::request_admission_projected::replay(|case| {
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
            requester_matches_session_scope: case.requester_matches_session_scope,
            current_approval: case.current_approval,
            exact_generation: case.exact_generation,
            authorization_fresh: case.authorization_fresh,
            runtime_evidence_present: case.runtime_evidence_present,
            runtime_source_kind,
            target_runtime_attestation_valid: case.target_runtime_attestation_valid,
            source_binding_current: case.source_binding_current,
            trigger_config_document_binding_current: case.trigger_config_document_binding_current,
            source_document_binding_current: case.source_document_binding_current,
            target_policy_allows: case.target_policy_allows,
            native_create_authorized: case.native_create_authorized,
            hop_within_bound: case.hop_within_bound,
        };
        let admitted = final_claim_admission_disposition(true, observation)
            == AgentRequestAdmissionDisposition::Admit;
        if admitted {
            admitted_kinds.borrow_mut().insert(kind.as_str());
        } else if !case.hop_within_bound {
            refused_over_hop_kinds.borrow_mut().insert(kind.as_str());
        }
        gents_lean_vocab::families::request_admission_projected::Admission {
            admitted,
            disposition: final_claim_admission_disposition(case.observation_available, observation)
                .as_str()
                .to_string(),
        }
    });
    for kind in [
        AgentRequestAdmissionKind::Enrollment,
        AgentRequestAdmissionKind::Client,
        AgentRequestAdmissionKind::LocalSelf,
        AgentRequestAdmissionKind::RuntimeInternal,
        AgentRequestAdmissionKind::Peer,
    ] {
        assert!(
            admitted_kinds.borrow().contains(kind.as_str()),
            "Lean emitted no admitted {} row",
            kind.as_str()
        );
    }
    for kind in [
        AgentRequestAdmissionKind::Client,
        AgentRequestAdmissionKind::LocalSelf,
        AgentRequestAdmissionKind::Peer,
    ] {
        assert!(
            refused_over_hop_kinds.borrow().contains(kind.as_str()),
            "Lean emitted no hop-bound refusal for {}",
            kind.as_str()
        );
    }
}
