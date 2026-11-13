use gents_lean_vocab::{lean_agent_request_admission_cases, lean_contract_snapshot};
use gents_protocol::request_admission::{
    AgentRequestAdmissionDisposition, AgentRequestAdmissionKind, AgentRequestAdmissionObservation,
    RuntimeInternalSourceKind,
};

#[test]
fn generated_agent_request_admission_cases_match_shared_projector() {
    let cases = lean_agent_request_admission_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no AgentRequest admission cases"
    );
    let mut admitted_kinds = std::collections::BTreeSet::new();
    let mut refused_over_hop_kinds = std::collections::BTreeSet::new();
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
        let actual =
            gents_session::request::admission::final_claim_admission_disposition(true, observation)
                == AgentRequestAdmissionDisposition::Admit;
        assert_eq!(actual, case.expected_admitted, "{}", case.name);
        let actual_disposition =
            gents_session::request::admission::final_claim_admission_disposition(
                case.observation_available,
                observation,
            );
        assert_eq!(
            actual_disposition.as_str(),
            case.expected_disposition,
            "{}",
            case.name
        );
        if case.expected_admitted {
            admitted_kinds.insert(kind.as_str());
        } else if !case.hop_within_bound {
            refused_over_hop_kinds.insert(kind.as_str());
        }
    }
    for kind in [
        AgentRequestAdmissionKind::Enrollment,
        AgentRequestAdmissionKind::Client,
        AgentRequestAdmissionKind::LocalSelf,
        AgentRequestAdmissionKind::RuntimeInternal,
        AgentRequestAdmissionKind::Peer,
    ] {
        assert!(
            admitted_kinds.contains(kind.as_str()),
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
            refused_over_hop_kinds.contains(kind.as_str()),
            "Lean emitted no hop-bound refusal for {}",
            kind.as_str()
        );
    }
}

#[test]
fn generated_causal_hop_cases_match_native_materializer() {
    let contract = lean_contract_snapshot();
    assert_eq!(contract.causal_hop_default_max_request_hop, None);
}
