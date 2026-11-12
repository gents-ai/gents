use gents_protocol::request_admission::{AgentRequestAdmissionDisposition, AgentRequestAdmissionKind, AgentRequestAdmissionObservation, RuntimeInternalSourceKind};
use gents_lean_vocab::{lean_agent_request_admission_cases, lean_causal_hop_contract};

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
    use gents_protocol::request_lifecycle::{
        next_request_hop, request_hop_within_bound, RequestHopCapObservation, RequestHopCause,
    };

    let contract = lean_causal_hop_contract();
    assert_eq!(contract.default_max_request_hop, None);
    assert!(contract
        .cap_observation_cases
        .iter()
        .any(|case| !case.visible));
    for case in &contract.cap_observation_cases {
        let observation = if case.visible {
            RequestHopCapObservation::Visible(case.cap)
        } else {
            RequestHopCapObservation::Unavailable
        };
        assert_eq!(observation.admit(case.hop), case.expected, "{}", case.name);
        assert_eq!(observation.bound(), case.expected_bound, "{}", case.name);
        if case.hop > 0 {
            assert_eq!(
                request_hop_within_bound(observation.bound(), case.hop),
                case.expected.unwrap_or(false),
                "{}",
                case.name
            );
        }
    }
    let cause = |name: &str, cause_hop: Option<u32>| match name {
        "root" => RequestHopCause::Root,
        "cross_session" => RequestHopCause::CrossSession {
            cause_hop: cause_hop.expect("cross-session step names its cause hop"),
        },
        "return" => RequestHopCause::Return,
        "continuation" => RequestHopCause::Continuation,
        other => panic!("unknown Lean causal-hop cause {other:?}"),
    };
    assert!(!contract.step_cases.is_empty());
    for case in &contract.step_cases {
        let hop = next_request_hop(cause(&case.cause, case.cause_hop), case.own_predecessor_hop);
        assert_eq!(hop, case.expected_hop, "{}", case.name);
        assert_eq!(
            request_hop_within_bound(case.max_request_hop, hop),
            case.expected_admitted,
            "{}",
            case.name
        );
    }
    assert!(!contract.chain_cases.is_empty());
    for chain in &contract.chain_cases {
        let mut hop = 0;
        let mut hops = Vec::new();
        let mut admitted = Vec::new();
        for step in &chain.steps {
            hop = match step.kind.as_str() {
                "cross_session" => next_request_hop(
                    RequestHopCause::CrossSession { cause_hop: hop },
                    step.own_predecessor_hop
                        .expect("cross-session step names its own predecessor hop"),
                ),
                other => next_request_hop(cause(other, None), hop),
            };
            hops.push(hop);
            admitted.push(request_hop_within_bound(chain.max_request_hop, hop));
        }
        assert_eq!(hops, chain.expected_hops, "{}", chain.name);
        assert_eq!(admitted, chain.expected_admitted, "{}", chain.name);
    }
    assert!(!contract.call_cases.is_empty());
    for call in &contract.call_cases {
        let (mut a, mut b) = (0, 0);
        let mut hops = Vec::new();
        for event in &call.events {
            let (target, cause) = match event.as_str() {
                "a_sends_b" => (&mut b, RequestHopCause::CrossSession { cause_hop: a }),
                "b_sends_a" => (&mut a, RequestHopCause::CrossSession { cause_hop: b }),
                "return_to_a" => (&mut a, RequestHopCause::Return),
                "return_to_b" => (&mut b, RequestHopCause::Return),
                other => panic!("unknown Lean call event {other:?}"),
            };
            *target = next_request_hop(cause, *target);
            hops.push(*target);
        }
        let admitted = hops
            .iter()
            .map(|hop| request_hop_within_bound(call.max_request_hop, *hop))
            .collect::<Vec<_>>();
        assert_eq!(hops, call.expected_hops, "{}", call.name);
        assert_eq!(admitted, call.expected_admitted, "{}", call.name);
    }
    assert!(!contract.interrupt_cases.is_empty());
    for case in &contract.interrupt_cases {
        assert_eq!(
            gents_session::session::message::agent_interrupt_allowed(
                &case.caller_session,
                &case.target_session,
                case.target_origin_cause.as_deref(),
                case.same_node,
                case.task_triggered,
            ),
            case.expected_allowed,
            "{}",
            case.name
        );
    }
}

